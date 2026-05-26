pub fn timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[macro_export]
macro_rules! log_event {
    ($level:expr, $event:expr) => {
        worker::console_log!(
            "[{}] level={} event={}",
            $crate::logger::timestamp(),
            $level,
            $event
        )
    };
    ($level:expr, $event:expr, $($arg:tt)*) => {
        worker::console_log!(
            "[{}] level={} event={} {}",
            $crate::logger::timestamp(),
            $level,
            $event,
            format!($($arg)*)
        )
    };
}
