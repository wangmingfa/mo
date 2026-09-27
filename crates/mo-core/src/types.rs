//! 类型知识：**一个后缀「是什么」只在这一处回答**。
//!
//! ## 为什么要有这个模块
//!
//! 同一件事原本在三个 crate 各写一份字符串匹配：
//!
//! * `mo_core::view::kind_group_of` —— 分组（按类型分组时落在哪一组）；
//! * `mo_preview` 的 `is_image` / `is_pdf` / `kind_by_ext` —— 预览用什么方式呈现；
//! * `mo_app::icon` 的 `is_package_ext` / `is_per_file_ext` —— 图标能不能按类型共享。
//!
//! 三份表各长各的，于是出现「列表把 `.svg` 归进图片组、预览却当它是文本」这种**自相矛盾
//! 的显示**（用户看到的是：同一文件在分组里叫图片、双击预览却是源码）。收在一处之后，
//! 每一问仍然独立回答（它们本就是三个不同的问题），但**答案互相冲突时一眼看得见**，
//! 改与不改都是有意识的决定，而不是「另一张表没人记得同步」。
//!
//! ## 三条规则
//!
//! 1. 判据一律是**小写、不含点**的扩展名（调用方负责折小写；`icon_key` 与 `kind_by_ext`
//!    今天就是这么做的）。
//! 2. 这里的三个函数都是**纯匹配、无 IO 无锁**——它们在列目录的热路径上被每个条目调一次
//!    （两万条目录 × 若干问），所以任何「去读表 / 加锁 / 查数据库」的实现都不许进来。
//! 3. 以后插件贡献的类型规则（见 `devlog/plugin-system.md` §4.1）**先于**这里查：
//!    作者更懂自己的格式，允许覆盖内置答案。这也是为什么这三问被拆成三个函数而不是
//!    合成一个大 struct——覆盖的粒度是「某一问」，不是整条。
//!
//! ## 已知且刻意保留的两处不一致
//!
//! * `.svg`：分组算**图片**（`GroupKey::Image`），预览算**文本**（渲染 SVG 要浏览器或
//!   光栅器，`img()` 直接吃不下去，退化成看源码比「一张破图」有用）。
//! * `.avif`：预览算**图片**，分组还没跟上（留在 `Other`）。这一处是纯遗漏，
//!   但改它要连带动分组的顺序表与既有断言，留到有回归网的时候单独改。
//!
//! 两处都由 `svg_and_avif_are_the_known_cross_axis_disagreements` 这条测试钉住——
//! 有人（或某个模型）「顺手统一」了它们，测试会红，逼他去看这段注释。

use crate::view::GroupKey;

/// 预览该用什么方式呈现一个文件。
///
/// 与 `mo_preview::PreviewKind` 不是一回事：那边混着「目录 / 空文件 / 二进制」这些
/// **要靠 stat 与读头部**才知道的态，这里只管后缀能定下来的那部分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewClass {
    /// 图片：原路径交给 UI 的 `img()` 直接加载。
    Image,
    /// PDF：首页要先过一遍平台渲染，UI 走两段式（先占位、图后到）。
    Pdf,
    /// Markdown。
    Markdown,
    /// JSON。
    Json,
    /// 源代码（着色提示）。
    Code,
    /// 普通文本（也是未知后缀的兜底）。
    Text,
}

/// 这个后缀的图标能不能**按类型共享**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconShare {
    /// 同扩展名共用一张类型图标（绝大多数）。
    ByType,
    /// 系统里的**包**：每个包自带图标，必须按路径问。
    ByPathPackage,
    /// 每个条目自带图标（快捷方式 / 可执行文件）。
    ///
    /// ⚠️ 只有 Windows 上成立，调用方（`mo_app::icon`）带 cfg 判；macOS 的快捷方式是
    /// `.app` 包或 Finder alias（后者类型图标本就是通用那张），走 `ByType` 才对。
    ByPathExecutable,
}

/// 图片族（分组用）：`svg` 在这一档——分组角度它确实是「图」，用户按类型翻找时
/// 想和别的图片放一起。预览那一问单独回答，见模块头。
const IMAGE_GROUP_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "heic", "svg",
];

/// 文档族（分组用）。
const DOCUMENT_GROUP_EXTS: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv", "rtf", "pages",
    "numbers", "key", "odt", "ods",
];

/// 音视频族（分组用）。
const MEDIA_GROUP_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "mp3", "wav", "flac", "aac", "m4a", "ogg",
];

/// 归档族（分组用）。`tar.gz` 这类双后缀按**最后一段**（`gz`）算，与既有行为一致。
const ARCHIVE_GROUP_EXTS: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "dmg", "iso",
];

/// 预览能直接 `img()` 加载的图片族。
///
/// 与 [`IMAGE_GROUP_EXTS`] 差两个：多 `avif`（`image` 解得动），少 `svg`（见模块头）。
const PREVIEW_IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "avif", "heic",
];

/// 源代码族（预览按 `Code` 着色）。
const CODE_EXTS: &[&str] = &[
    "rs", "py", "js", "ts", "jsx", "tsx", "c", "cpp", "h", "hpp", "cc", "java", "go", "sh", "bash",
    "zsh", "toml", "yaml", "yml", "cfg", "conf", "ini", "css", "html", "htm", "xml", "lua", "rb",
    "php", "sql",
];

/// 系统里是**包**的后缀：每个包自带图标（macOS 的 `.app` 那一族）。
///
/// 目录形态的包在 `icon_key` 里已被 `is_dir` 拦住，这张表是给**符号链接形态**兜底的——
/// 指向 `.app` 的快捷方式在 Mo 里是文件，不特判就会一堆 app 共用第一个被问到的那张图。
const PACKAGE_EXTS: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "plugin",
    "kext",
    "xpc",
    "appex",
    "prefpane",
    "qlgenerator",
    "mdimporter",
    "saver",
    "scptd",
];

/// Windows 上**每个条目自带图标**的后缀（见 [`IconShare::ByPathExecutable`] 的告警）。
///
/// 实测（32px 逐张比对像素指纹）：`.lnk` 的类型图标是一张通用白纸，而桌面上三个快捷方式
/// 各自拿到 atlas 的 `.ico` / PotPlayer / VS Code 图标；`.exe` 同理（`notepad.exe`、
/// `cmd.exe` 各一张，只有没内嵌图标的 `ping.exe` 才等于类型图）。`.url` 与 `.lnk` 同为
/// 快捷方式、`.scr` 本质是换了后缀的 `.exe`，按同一条规律收进来（这两条无样本，未实测）。
const PER_FILE_EXTS: &[&str] = &["lnk", "url", "exe", "scr"];

fn in_set(set: &[&str], ext: &str) -> bool {
    set.contains(&ext)
}

/// 按类型分组时，这个后缀落哪一组。未知 = [`GroupKey::Other`]。
///
/// `Folder` 不在这里答——那要条目自己是不是目录，调用处按 `kind` 判。
pub fn group_of(ext: &str) -> GroupKey {
    if in_set(IMAGE_GROUP_EXTS, ext) {
        GroupKey::Image
    } else if in_set(DOCUMENT_GROUP_EXTS, ext) {
        GroupKey::Document
    } else if in_set(MEDIA_GROUP_EXTS, ext) {
        GroupKey::Media
    } else if in_set(ARCHIVE_GROUP_EXTS, ext) {
        GroupKey::Archive
    } else {
        GroupKey::Other
    }
}

/// 预览该走哪条路。未知 = [`PreviewClass::Text`]（读到非 UTF-8 头部再退二进制，
/// 那是 `mo_preview` 的事，不靠后缀）。
pub fn preview_of(ext: &str) -> PreviewClass {
    if in_set(PREVIEW_IMAGE_EXTS, ext) {
        PreviewClass::Image
    } else if ext == "pdf" {
        PreviewClass::Pdf
    } else if ext == "md" || ext == "markdown" {
        PreviewClass::Markdown
    } else if ext == "json" {
        PreviewClass::Json
    } else if in_set(CODE_EXTS, ext) {
        PreviewClass::Code
    } else {
        PreviewClass::Text
    }
}

/// 这个后缀的图标能不能按类型共享。
pub fn icon_share_of(ext: &str) -> IconShare {
    if in_set(PACKAGE_EXTS, ext) {
        IconShare::ByPathPackage
    } else if in_set(PER_FILE_EXTS, ext) {
        IconShare::ByPathExecutable
    } else {
        IconShare::ByType
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三问各自的基本形状。
    #[test]
    fn each_question_answers_on_its_own_axis() {
        assert_eq!(group_of("png"), GroupKey::Image);
        assert_eq!(group_of("md"), GroupKey::Document);
        assert_eq!(group_of("mp4"), GroupKey::Media);
        assert_eq!(group_of("zip"), GroupKey::Archive);
        // 代码族在分组上是 Other：按类型翻文件的人不想看到一栏「.rs 也算文档」。
        assert_eq!(group_of("rs"), GroupKey::Other);
        assert_eq!(group_of("不存在的后缀"), GroupKey::Other);

        assert_eq!(preview_of("png"), PreviewClass::Image);
        assert_eq!(preview_of("pdf"), PreviewClass::Pdf);
        assert_eq!(preview_of("markdown"), PreviewClass::Markdown);
        assert_eq!(preview_of("json"), PreviewClass::Json);
        assert_eq!(preview_of("rs"), PreviewClass::Code);
        assert_eq!(preview_of("log"), PreviewClass::Text);

        assert_eq!(icon_share_of("app"), IconShare::ByPathPackage);
        assert_eq!(icon_share_of("lnk"), IconShare::ByPathExecutable);
        assert_eq!(icon_share_of("txt"), IconShare::ByType);
    }

    /// ⚠️ 钉住那两处**跨轴矛盾**（模块头有解释）：它们是已记录的现状，不是待修的 bug
    /// 现场。谁把它们「统一」了，这条就红——去看模块头那段再决定要不要改判据。
    #[test]
    fn svg_and_avif_are_the_known_cross_axis_disagreements() {
        assert_eq!(group_of("svg"), GroupKey::Image);
        assert_eq!(preview_of("svg"), PreviewClass::Text);
        assert!(
            !PREVIEW_IMAGE_EXTS.contains(&"svg"),
            "svg 进预览图片族 = UI 吃不下"
        );

        assert_eq!(preview_of("avif"), PreviewClass::Image);
        assert_eq!(group_of("avif"), GroupKey::Other, "avif 的分组还没跟上");
    }

    /// 集合不许重叠：一个后缀在两问的同一轴上只能属于一族。
    ///
    /// 分组那四族互斥是**顺序**决定的（`group_of` 先图片后文档…），今天靠数组不重叠
    /// 保证；将来加后缀的人未必看得见顺序依赖。
    #[test]
    fn group_sets_are_disjoint() {
        for a in [
            IMAGE_GROUP_EXTS,
            DOCUMENT_GROUP_EXTS,
            MEDIA_GROUP_EXTS,
            ARCHIVE_GROUP_EXTS,
        ] {
            for b in [
                IMAGE_GROUP_EXTS,
                DOCUMENT_GROUP_EXTS,
                MEDIA_GROUP_EXTS,
                ARCHIVE_GROUP_EXTS,
            ] {
                if std::ptr::eq(a, b) {
                    continue;
                }
                for e in a {
                    assert!(
                        !b.contains(e),
                        "后缀「{e}」同时出现在两个分组族里，`group_of` 的结果就取决于顺序"
                    );
                }
            }
        }
    }

    /// 判据一律小写：大写在调用方就被折掉了，这里喂大写不该命中。
    #[test]
    fn lookups_are_lowercase_only() {
        assert_eq!(group_of("PNG"), GroupKey::Other);
        assert_eq!(preview_of("PDF"), PreviewClass::Text);
    }
}
