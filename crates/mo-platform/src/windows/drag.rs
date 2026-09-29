#![allow(unsafe_code)]
//! 把文件**拖出** Mo：Windows 的 OLE 拖拽源（`DoDragDrop`）。
//!
//! 与 §「拖进来」是两条完全独立的链路。进来那半 gpui 已经做好了（`IDropTarget`），
//! 出去这半 gpui 的 Windows 后端是空的——它的 `WindowPlatform::start_external_drag`
//! 用的是 trait 默认的 `false`，而触发那条路径还要求应用用 gpui 的 `on_drag` 起拖
//! （Mo 的应用内拖拽是自己拿鼠标事件写的，gpui 根本不知道有一次拖拽正在进行）。
//! 所以这一层只能自己接 OLE。
//!
//! ⚠️ **绝不能在 UI 线程上等这次拖拽**：`DoDragDrop` 是模态的——它自己起一个消息
//! 循环一直泵到落子为止。而 Mo 的拖拽起点是 gpui 的输入回调，那时 `App` 内部的
//! `RefCell` 正被可变借用；OLE 的循环里一条 `WM_GPUI_TASK_*` 被派发就会二次
//! `borrow_mut`，直接 panic。这里为此**单独开一条线程**做 STA：OLE 只泵这条线程的
//! 队列，碰不到 gpui 的隐藏窗口消息，UI 线程照常渲染。结论从 `on_done` 回调给出，
//! 而它**跑在拖拽线程上**——调用方要回 UI 线程得自己接（Mo 用一条 oneshot  channel，
//! 见 `mo_ui::app::RootView::hand_off_drag_to_os`）。

use std::path::PathBuf;

use windows::core::{implement, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK,
};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_MOVE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_DataObject, ILCreateFromPathW, ILFree, IShellItemArray, SHCreateShellItemArrayFromIDLists,
};

use super::ComGuard;

/// 起一次「拖出到系统」：答 `false` = 线程都起不来（这批路径压根没交出去）。
///
/// `on_done` 收到拖拽结果：`None` = 没落地（用户取消 / 起拖失败），
/// `Some(true)` = 系统按**移动**收的，`Some(false)` = 复制。
///
/// 「移动」时调用方必须把源删掉——OLE 的约定是目标只负责在原地放一份，搬走源是
/// 源端的活（跨盘移动时资源管理器就是只复制、然后回 `DROPEFFECT_MOVE` 让源端删）。
/// ⚠️ 当前只声明复制（见 `do_drag` 里的注释），`Some(true)` 实际收不到；分支留给
/// 将来开移动语义。
///
/// ⚠️ `on_done` 在拖拽线程上被调，不在 UI 线程。
pub fn begin(paths: Vec<PathBuf>, on_done: Box<dyn FnOnce(Option<bool>) + Send>) -> bool {
    if paths.is_empty() {
        return false;
    }
    std::thread::Builder::new()
        .name("mo-file-drag".to_string())
        .spawn(move || on_done(drag_thread(&paths)))
        .is_ok()
}

/// 一条全新的线程：自己认领 STA + OLE，跑完自己收干净。
fn drag_thread(paths: &[PathBuf]) -> Option<bool> {
    let _com = ComGuard::init();
    // `OleInitialize` 必须在这条线程上单独调：`DoDragDrop` 要的剪贴板 / 拖拽
    // 代理注册表都是 per-thread 的，gpui 在 UI 线程上做过一遍不算这里。
    if unsafe { OleInitialize(None) }.is_err() {
        return None;
    }
    let result = unsafe { do_drag(paths) };
    unsafe { OleUninitialize() };
    result
}

unsafe fn do_drag(paths: &[PathBuf]) -> Option<bool> {
    let data = file_data_object(paths)?;
    let source: IDropSource = DragSource.into();
    // **只声明复制**：与 macOS 侧（`macos.rs` 的 DRAG_MASK）同一决策——拖入通道
    // （gpui 的 drop）拿不到按键状态、恒按复制收，拖出若允许移动就会「拖出移动、
    // 拖入复制」不对等。移动语义等将来把按键状态透传进来再一起开。
    let mut effect = DROPEFFECT_COPY;
    let hr = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
    // `DRAGDROP_S_DROP` / `DRAGDROP_S_CANCEL` 都是成功段的码（`is_err` 为假），
    // 差别只在含义：一个真落了子，一个被用户取消。
    if hr.is_err() || hr == DRAGDROP_S_CANCEL {
        return None;
    }
    Some(effect == DROPEFFECT_MOVE)
}

/// 给这批路径造一个 shell 的 `IDataObject`。
///
/// 不自己写 `IDataObject`：走 shell 的 `IShellItemArray` → `BHID_DataObject`，交出来的
/// 那份除了 `CF_HDROP` 还带 `FileGroupDescriptor`（对方能看懂「一个文件夹里的若干
/// 文件」）和拖拽图像；自己手搓只给得到 `CF_HDROP`，落到资源管理器以外的目标上会
/// 明显掉相。
unsafe fn file_data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    let mut pidls: Vec<*const ITEMIDLIST> = Vec::with_capacity(paths.len());
    for path in paths {
        let wide = super::encode_wide(path);
        let pidl = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        if pidl.is_null() {
            // 这条路径 shell 不认（不存在 / 非法名）：整批放弃，别拖半份出去。
            for pidl in &pidls {
                ILFree(Some(*pidl));
            }
            return None;
        }
        pidls.push(pidl);
    }
    let array: windows::core::Result<IShellItemArray> = SHCreateShellItemArrayFromIDLists(&pidls);
    for pidl in &pidls {
        ILFree(Some(*pidl));
    }
    array.ok()?.BindToHandler(None, &BHID_DataObject).ok()
}

/// 拖拽源回调。OLE 在它的模态循环里反复问这两个问题。
#[implement(IDropSource)]
struct DragSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for DragSource_Impl {
    fn QueryContinueDrag(&self, escape_pressed: BOOL, key_state: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape_pressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        // 松手 = 落子。OLE 递来的按键态里带 `MK_LBUTTON`，但这条线程没有窗口、
        // 也没拿到鼠标捕获，按键位偶尔缺帧；两个判据都认定「已经抬起」才收，
        // 误判成「还按着」只是多问一轮，反过来早退会把文件掉在错误的目标上。
        let held_by_ole = key_state.0 & MK_LBUTTON.0 != 0;
        let held_by_os = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
        if held_by_ole || held_by_os {
            S_OK
        } else {
            DRAGDROP_S_DROP
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        // 让 OLE 用系统那套「+ 复制 / 箭头 移动」光标，自己不画东西。
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// 从 `IDataObject` 里把 `CF_HDROP` 那份路径表读出来。
///
/// 这就是**对方程序**读到的东西，所以它是单测的抓手：不真起一次拖拽（那要别的进程
/// 当落点、要真鼠标）也能验「递出去的路径对不对」。
#[cfg(test)]
fn hdrop_of(data: &IDataObject) -> Option<Vec<String>> {
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL};
    use windows::Win32::System::Memory::GlobalLock;
    use windows::Win32::System::Ole::ReleaseStgMedium;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    let format = FORMATETC {
        cfFormat: super::CF_HDROP as u16,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    if unsafe { data.QueryGetData(&format) } != S_OK {
        return None;
    }
    let mut medium: STGMEDIUM = unsafe { data.GetData(&format).ok()? };
    let out = unsafe {
        let handle = medium.u.hGlobal;
        let ptr = GlobalLock(handle);
        if ptr.is_null() {
            None
        } else {
            // `DROPFILES` 头之后才是路径表，但 `DragQueryFileW` 要的就是整个
            // HGLOBAL 的句柄，它自己会跳过头部。
            let drop = HDROP(handle.0);
            let count = DragQueryFileW(drop, u32::MAX, None) as usize;
            let mut names = Vec::with_capacity(count);
            for i in 0..count {
                // 不带缓冲问一次 = 这条路径的字符数（不含结尾 NUL）。
                let len = DragQueryFileW(drop, i as u32, None) as usize;
                let mut buf = vec![0u16; len + 1];
                if DragQueryFileW(drop, i as u32, Some(&mut buf)) == 0 {
                    continue;
                }
                let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                names.push(String::from_utf16_lossy(&buf[..end]));
            }
            Some(names)
        }
    };
    unsafe { ReleaseStgMedium(&mut medium) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 建 data object + 读 `CF_HDROP` 都要在 STA 线程上跑，而且**绝不碰** `DoDragDrop`
    /// （那要真的鼠标拖拽和别的进程的落点，headless 里做不了）。
    fn paths_on_sta_thread(dir: &Path, names: &[&str]) -> Vec<String> {
        let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
        let worker = std::thread::spawn(move || {
            let _com = ComGuard::init();
            if unsafe { OleInitialize(None) }.is_err() {
                return Vec::new();
            }
            let out = unsafe { file_data_object(&paths) }.and_then(|d| hdrop_of(&d));
            unsafe { OleUninitialize() };
            out.unwrap_or_default()
        });
        worker.join().unwrap()
    }

    #[test]
    fn data_object_offers_exactly_the_paths_we_handed_it() {
        let dir = std::env::temp_dir().join(format!("mo-drag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        std::fs::write(dir.join("one.txt"), b"1").unwrap();
        std::fs::write(dir.join("两个 字.txt"), b"2").unwrap();
        std::fs::create_dir_all(dir.join("folder")).unwrap();

        let got = paths_on_sta_thread(&dir, &["one.txt", "两个 字.txt", "folder"]);
        let mut want: Vec<String> = ["one.txt", "两个 字.txt", "folder"]
            .iter()
            .map(|n| dir.join(n).display().to_string())
            .collect();
        want.sort();
        let mut sorted = got.clone();
        sorted.sort();
        assert_eq!(
            sorted, want,
            "递出去的路径表必须与给它的完全一致（含中文名与目录）：实际 {got:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn data_object_declines_when_a_path_does_not_exist() {
        let dir = std::env::temp_dir().join(format!("mo-drag-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real.txt"), b"x").unwrap();
        // 一条真的、一条早已被删：宁可不拖，也不能拖出「一半成功一半 404」。
        let got = paths_on_sta_thread(&dir, &["real.txt", "ghost.txt"]);
        assert!(
            got.is_empty(),
            "路径不成立时不该递出半个文件表：实际 {got:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 直连 `QueryContinueDrag`：它只是「按键态 → 继续还是落子」的纯判断，
    /// 不必真起一次拖拽（那要别的进程当落点、要真鼠标）也能验。
    fn continue_drag(escape: bool, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        let source: IDropSource = DragSource.into();
        unsafe { source.QueryContinueDrag(BOOL(escape as i32), keys) }
    }

    #[test]
    fn drop_source_aborts_on_escape() {
        // 左键还按着但用户按了 Esc：认 Esc。这时落子会把文件丢在指针底下
        // 随便哪个目标上，而用户刚刚明确说了「不要」。
        assert_eq!(continue_drag(true, MK_LBUTTON), DRAGDROP_S_CANCEL);
    }

    #[test]
    fn drop_source_keeps_going_while_ole_reports_the_button_held() {
        // OLE 递来的按键位说还按着就继续——两条判据是「或」，宁可多问一轮，
        // 也不能提前落子把文件掉在半路上的某个窗口上。
        assert_eq!(continue_drag(false, MK_LBUTTON), S_OK);
    }
}
