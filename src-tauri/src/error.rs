use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("audio error: {0}")]
    Audio(String),
    #[error("transcription error: {0}")]
    Transcribe(String),
    #[error("vad error: {0}")]
    Vad(String),
    #[error("llm error: {0}")]
    Llm(String),
    #[error("injection error: {0}")]
    Inject(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("database error: {0}")]
    Db(String),
    #[error("download error: {0}")]
    Download(String),
    #[error("model error: {0}")]
    Model(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        AppError::Io(value.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        AppError::Config(value.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        AppError::Other(value.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
