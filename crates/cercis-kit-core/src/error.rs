//! 工具包的统一错误：**必须带上"怎么办"**，否则用户只看到一句报错。

use std::fmt;

/// 错误：一句说清发生了什么，再加一句建议怎么处理。
#[derive(Debug, Clone)]
pub struct KitError {
    pub message: String,
    pub hint: String,
}

impl KitError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: String::new(),
        }
    }

    /// 带处理建议的错误。
    pub fn with_hint(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: hint.into(),
        }
    }

    /// 无处理建议时的兜底：提示可以查帮助。
    pub fn hint_or_default(&self) -> &str {
        if self.hint.is_empty() {
            "加 --json 可获得机器可读的输出；用 --help 查看用法"
        } else {
            &self.hint
        }
    }
}

impl fmt::Display for KitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if !self.hint.is_empty() {
            write!(f, "（{}）", self.hint)?;
        }
        Ok(())
    }
}

impl std::error::Error for KitError {}

pub type Result<T> = std::result::Result<T, KitError>;

/// 快速构造错误。
pub fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(KitError::new(message))
}

/// 快速构造带建议的错误。
pub fn err_hint<T>(message: impl Into<String>, hint: impl Into<String>) -> Result<T> {
    Err(KitError::with_hint(message, hint))
}

impl From<std::io::Error> for KitError {
    fn from(e: std::io::Error) -> Self {
        KitError::new(format!("文件操作失败：{e}"))
    }
}

impl From<serde_json::Error> for KitError {
    fn from(e: serde_json::Error) -> Self {
        KitError::new(format!("JSON 解析失败：{e}"))
    }
}

impl From<csv::Error> for KitError {
    fn from(e: csv::Error) -> Self {
        KitError::new(format!("表格解析失败：{e}"))
    }
}
