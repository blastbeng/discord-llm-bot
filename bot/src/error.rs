//! Enhanced error handling module with structured error types and comprehensive logging support.

use std::fmt;
use thiserror::Error;

/// Comprehensive error types for the Discord bot
#[derive(Error, Debug)]
pub enum BotError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("TTS service error: {message}. Voice: {voice}, fallback: {fallback}")]
    TtsService {
        message: String,
        voice: String,
        fallback: bool,
    },

    #[error("Voice connection error: {0}")]
    VoiceConnection(String),

    #[error("Permission denied: {0}")]
    PermissionError(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("External API error ({api}): {message}")]
    ExternalApi {
        api: String,
        message: String,
    },

    #[error("File operation error: {0}")]
    FileError(String),

    #[error("Command execution error: {context}: {message}")]
    CommandError {
        context: String,
        message: String,
    },

    #[error("Rate limit exceeded for user {user_id}. Retry after {retry_after}s")]
    RateLimitExceeded {
        user_id: u64,
        retry_after: f32,
    },

    #[error("Unexpected error in {context}: {message}")]
    UnexpectedError {
        context: String,
        message: String,
    },
}

impl BotError {
    /// Create a new database error
    pub fn database<E: fmt::Display>(error: E) -> Self {
        BotError::Database(error.to_string())
    }

    /// Create a TTS service error with fallback information
    pub fn tts_error(message: &str, voice: &str, fallback: bool) -> Self {
        BotError::TtsService {
            message: message.to_string(),
            voice: voice.to_string(),
            fallback,
        }
    }

    /// Create a permission error with detailed context
    pub fn permission_error(context: &str, required_permissions: &[&str]) -> Self {
        let perms = required_permissions.join(", ");
        BotError::PermissionError(format!(
            "Required permissions not met in {}: {}. Missing or insufficient access.",
            context, perms
        ))
    }

    /// Create a configuration error with environment details
    pub fn config_error(key: &str, value: Option<&str>, message: &str) -> Self {
        let env_info = if let Some(v) = value {
            format!("({}={})", key, v)
        } else {
            format!("{} (not set)", key)
        };
        BotError::Configuration(format!("{} {}", message, env_info))
    }

    /// Create an external API error with retry information
    pub fn api_error(api: &str, message: &str, retry_after: Option<f32>) -> Self {
        let error = if let Some(retry) = retry_after {
            BotError::ExternalApi {
                api: api.to_string(),
                message: format!("{} (retry after {:.1}s)", message, retry),
            }
        } else {
            BotError::ExternalApi {
                api: api.to_string(),
                message: message.to_string(),
            }
        };
        error
    }

    /// Check if the error requires user action
    pub fn requires_user_action(&self) -> bool {
        matches!(
            self,
            BotError::PermissionError(_)
                | BotError::RateLimitExceeded { .. }
                | BotError::CommandError { .. }
        )
    }

    /// Get error context for logging and monitoring
    pub fn get_context(&self) -> String {
        match self {
            BotError::Database(msg) => format!("DB: {}", msg),
            BotError::TtsService { message, voice, fallback } => {
                format!("TTS [{}{}]: {}", voice, if *fallback { " (fallback)" } else { "" }, message)
            }
            BotError::VoiceConnection(msg) => format!("VOICE: {}", msg),
            BotError::PermissionError(msg) => format!("PERMISSIONS: {}", msg),
            BotError::Configuration(msg) => format!("CONFIG: {}", msg),
            BotError::ExternalApi { api, message } => format!("{API}: {}", message),
            BotError::FileError(msg) => format!("FILE: {}", msg),
            BotError::CommandError { context, message } => format!("[{}] {}", context, message),
            BotError::RateLimitExceeded { user_id, retry_after } => {
                format!("RATE_LIMIT(user_{}): {:.1}s remaining", user_id, *retry_after)
            }
            BotError::UnexpectedError { context, message } => format!("[{}] {}", context, message),
        }
    }

    /// Determine error severity level
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            BotError::Database(_) | BotError::TtsService { .. } | BotError::ExternalApi { .. } => {
                ErrorSeverity::High
            }
            BotError::VoiceConnection(_)
            | BotError::PermissionError(_)
            | BotError::Configuration(_)
            | BotError::FileError(_) => ErrorSeverity::Medium,
            BotError::CommandError { .. }
            | BotError::RateLimitExceeded { .. }
            | BotError::UnexpectedError { .. } => ErrorSeverity::Info,
        }
    }

    /// Generate user-friendly error message with context
    pub fn to_user_message(&self) -> String {
        let base_msg = self.to_string();
        match self.severity() {
            ErrorSeverity::High => format!("⚠️ {}", base_msg),
            ErrorSeverity::Medium => format!("📋 {}", base_msg),
            ErrorSeverity::Info => format!("ℹ️ {}", base_msg),
        }
    }
}

/// Error severity levels for prioritization and monitoring
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self {
            ErrorSeverity::Critical => "CRITICAL",
            ErrorSeverity::High => "HIGH",
            ErrorSeverity::Medium => "MEDIUM",
            ErrorSeverity::Low => "LOW",
            ErrorSeverity::Info => "INFO",
        };
        write!(f, "{}", level)
    }
}

/// Logging utility for structured logging with context
pub struct Logger;

impl Logger {
    /// Log information message with context
    pub fn info<T: fmt::Display>(context: &str, message: T) {
        log::info!("[{}] {}", context, message);
    }

    /// Log warning message with optional details
    pub fn warn<T: fmt::Display>(context: &str, message: T, details: Option<&str>) {
        if let Some(detail) = details {
            log::warn!(
                "[{}] {}: {}",
                context,
                message,
                detail
            );
        } else {
            log::warn!("[{}] {}", context, message);
        }
    }

    /// Log error with stack trace information
    pub fn error<T: fmt::Display>(context: &str, error: T) {
        log::error!("[{}] ERROR: {}", context, error);
    }

    /// Log debug message with detailed information
    pub fn debug<T: fmt::Display>(context: &str, message: T) {
        log::debug!("[{}] {}", context, message);
    }

    /// Create a structured log entry for command execution
    pub fn log_command_execution(
        user_id: u64,
        command: &str,
        parameters: Option<&serde_json::Value>,
        duration_ms: u64,
        success: bool,
    ) {
        let status = if success { "SUCCESS" } else { "FAILED" };
        let params_summary = parameters.map(|p| p.to_string()).unwrap_or_default();

        log::info!(
            "[COMMAND_EXECUTION] user={}, command={}, duration={}ms, status={}, parameters={}",
            user_id,
            command,
            duration_ms,
            status,
            params_summary
        );
    }

    /// Log performance metrics with key measurements
    pub fn log_performance(
        metric_name: &str,
        value: f64,
        unit: &str,
        context: Option<&str>,
    ) {
        let ctx = context.unwrap_or("GENERAL");
        log::info!(
            "[PERFORMANCE] {}={:.2}{} [{}] ",
            metric_name,
            value,
            unit,
            ctx
        );
    }

    /// Log system resource usage metrics
    pub fn log_resources(cpu_usage: f64, memory_usage: f64, active_connections: u32) {
        log::info!(
            "[SYSTEM_RESOURCES] CPU: {:.1}%, Memory: {:.1}%, Connections: {}",
            cpu_usage,
            memory_usage,
            active_connections
        );
    }

    /// Log TTS operation with voice details
    pub fn log_tts_operation(voice: &str, operation: &str, duration_ms: u64, success: bool) {
        let status = if success { "SUCCESS" } else { "COMPLETED_WITH_WARNINGS" };
        Self::info(
            format!("TTS-{}", voice).as_str(),
            format!(
                "{} completed in {}ms (status: {})",
                operation,
                duration_ms,
                status
            ),
        );
    }

    /// Log database operations with query details
    pub fn log_database_operation(
        operation_type: &str,
        table: &str,
        records_affected: u64,
        execution_time_ms: u64,
    ) {
        Self::info(
            "DATABASE",
            format!(
                "{} on {} - Records affected: {}, Execution time: {}ms",
                operation_type,
                table,
                records_affected,
                execution_time_ms
            ),
        );
    }

    /// Log cache operations and hit/miss statistics
    pub fn log_cache_operation(
        cache_name: &str,
        operation: &str,
        hits: u64,
        misses: u64,
        hit_rate: f64,
    ) {
        Self::info(
            format!("CACHE-{}", cache_name).as_str(),
            format!(
                "{} - Hits: {}, Misses: {}, Hit Rate: {:.1}%",
                operation, hits, misses, hit_rate * 100.0
            ),
        );
    }

    /// Log external API interactions with response details
    pub fn log_api_interaction(
        api_name: &str,
        endpoint: &str,
        status_code: u16,
        latency_ms: u64,
        success: bool,
    ) {
        let status = if success { "SUCCESS" } else { "PARTIAL_SUCCESS" };
        Self::info(
            api_name,
            format!(
                "{} - Endpoint: {}, Status: {} ({}ms), Result: {}",
                api_name,
                endpoint,
                status_code,
                latency_ms,
                status
            ),
        );
    }
}

/// Error tracking and statistics collection
pub struct ErrorTracker {
    total_errors: std::sync::atomic::AtomicU64,
    errors_by_type: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    recent_errors: std::sync::Mutex<Vec<(std::time::SystemTime, BotError)>>,
}

impl ErrorTracker {
    pub fn new() -> Self {
        Self {
            total_errors: std::sync::atomic::AtomicU64::new(0),
            errors_by_type: std::sync::Mutex::new(std::collections::HashMap::new()),
            recent_errors: std::sync::Mutex::new(Vec::with_capacity(100)),
        }
    }

    /// Record an error occurrence
    pub fn record_error(&self, error: &BotError) {
        let timestamp = std::time::SystemTime::now();
        
        // Update total error count
        self.total_errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // Track errors by type
        let mut errors_by_type = self.errors_by_type.lock().unwrap();
        let error_type = format!("{:?}", error);
        *errors_by_type.entry(error_type).or_insert(0) += 1;

        // Store recent error for monitoring
        let mut recent_errors = self.recent_errors.lock().unwrap();
        if recent_errors.len() >= 100 {
            recent_errors.remove(0);
        }
        recent_errors.push((timestamp, error.clone()));
    }

    /// Get current error statistics
    pub fn get_statistics(&self) -> serde_json::Value {
        let total = self.total_errors.load(std::sync::atomic::Ordering::SeqCst);
        
        let errors_by_type = self.errors_by_type.lock().unwrap();
        let recent_count = self.recent_errors.lock().unwrap().len() as u64;

        serde_json::json!({
            "total_errors": total,
            "errors_by_type": *errors_by_type,
            "recent_error_count": recent_count,
            "last_updated": chrono::Utc::now().to_rfc3339()
        })
    }

    /// Get recent errors with optional time filter
    pub fn get_recent_errors(&self, hours: u64) -> Vec<(std::time::SystemTime, BotError)> {
        let now = std::time::SystemTime::now();
        let cutoff = now - std::time::Duration::from_hours(hours as i64);

        self.recent_errors
            .lock()
            .unwrap()
            .iter()
            .filter(|(timestamp, _)| *timestamp >= cutoff)
            .cloned()
            .collect()
    }
}

impl Default for ErrorTracker {
    fn default() -> Self {
        Self::new()
    }
}

// Re-export commonly used types
pub use self::{BotError, ErrorSeverity, Logger, ErrorTracker};
