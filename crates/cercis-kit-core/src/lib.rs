#![forbid(unsafe_code)]
//! # Cercis 工具包共用库（cercis-kit-core）
//!
//! 基础工具包由**一组独立插件**组成（项目向导、通用录入、通用导入导出、通用体检……），
//! 每个插件是可以单独安装、单独授权、单独启停的软件包。它们共用下面这些能力：
//!
//! - [`kernel`]：与内核之间**唯一**的通道（命令行 + `--json`）
//! - [`model`]：行与输入表（CSV/TSV/JSON 读写、CSV 规则）
//! - [`checks`]：写入前的行级检查（列名、必填、主键、类型）
//! - [`error`]：带"怎么办"的错误
//!
//! 本库**不含任何界面代码**：界面适配由各插件在 app 端通过扩展点声明完成
//! （需求文档 §14.6：核心能力平台无关、只写一份）。
//!
//! 插件通过内核通道读写数据，因此无法绕过内核直接改数据文件——
//! 这是架构硬规则，也是本库只提供 [`kernel`] 一条数据通路的原因。

pub mod checks;
pub mod cli;
pub mod error;
pub mod kernel;
pub mod model;
pub mod refs;
pub mod types;

pub use error::{err, err_hint, KitError, Result};
pub use kernel::{Kernel, KernelRun, TableField, TableSchema};
pub use model::{InputTable, Row};
pub use types::{RuleSeverity, TypeCatalog, Verdict};

/// 插件输出协议版本（机器可读输出的 `schema` 字段）。
pub const OUTPUT_SCHEMA: &str = "cercis.plugin/1";

/// 基础工具包的整体版本（用于插件清单与报告）。
pub const KIT_VERSION: &str = env!("CARGO_PKG_VERSION");
