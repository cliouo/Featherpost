use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorCode {
    Unsupported,
    CredentialMissing,
    CredentialInvalid,
    Authentication,
    Forbidden,
    InvalidInput,
    NotFound,
    Conflict,
    RateLimited,
    Upstream,
    Network,
    Timeout,
    InvalidResponse,
    ResponseTooLarge,
    CreateUnknown,
    DeliveryUnknown,
    State,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::CredentialMissing => "credential_missing",
            Self::CredentialInvalid => "credential_invalid",
            Self::Authentication => "authentication_failed",
            Self::Forbidden => "forbidden",
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::Upstream => "upstream_error",
            Self::Network => "network_error",
            Self::Timeout => "timeout",
            Self::InvalidResponse => "invalid_response",
            Self::ResponseTooLarge => "response_too_large",
            Self::CreateUnknown => "create_unknown",
            Self::DeliveryUnknown => "delivery_unknown",
            Self::State => "state_error",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppError {
    code: ErrorCode,
    message: &'static str,
    detail: Option<String>,
}

impl AppError {
    pub const fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            detail: None,
        }
    }

    pub fn with_detail(code: ErrorCode, message: &'static str, detail: String) -> Self {
        Self {
            code,
            message,
            detail: Some(detail),
        }
    }

    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    pub const fn stable_code(&self) -> &'static str {
        self.code.as_str()
    }

    pub fn message(&self) -> &str {
        self.detail.as_deref().unwrap_or(self.message)
    }

    pub const fn invalid_input() -> Self {
        Self::new(ErrorCode::InvalidInput, "输入参数无效")
    }

    pub const fn invalid_response() -> Self {
        Self::new(ErrorCode::InvalidResponse, "NodeSeek Email 响应结构无效")
    }

    pub const fn state() -> Self {
        Self::new(ErrorCode::State, "插件状态不可用")
    }

    pub const fn possibly_applied(&self) -> bool {
        matches!(
            self.code,
            ErrorCode::Network
                | ErrorCode::Timeout
                | ErrorCode::InvalidResponse
                | ErrorCode::ResponseTooLarge
                | ErrorCode::Upstream
        )
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message())
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;
