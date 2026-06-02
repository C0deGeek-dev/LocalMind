use thiserror::Error;

pub type ContractResult<T> = Result<T, ContractError>;

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("confidence must be between 0.0 and 1.0, got {value}")]
    InvalidConfidence { value: f32 },
}
