//! Logging macros.

/// Logs at a level chosen at runtime: `log!(level, "message" …)`.
///
/// Takes the same arguments as [`info!`](crate::info!) after the level; `logger:` and `target:` come
/// before it:
///
/// ```
/// use dl_log::{Level, Logger, MemorySink, TextFormat, log};
///
/// let mem = MemorySink::new().with_format(TextFormat::new().with_time(false));
/// let lg = Logger::builder().sink(mem.clone()).build();
/// let level = if 503 >= 500 { Level::Error } else { Level::Info };
/// log!(logger: lg, level, "upstream answered {}", 503; upstream = "payments");
/// assert_eq!(mem.lines(), ["ERROR upstream answered 503 upstream=payments"]);
/// ```
#[macro_export]
macro_rules! log {
    (logger: $logger:expr, target: $target:expr, $level:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($logger) ($target) ($level)) [] $($rest)+)
    };
    (logger: $logger:expr, $level:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($logger) (::core::module_path!()) ($level)) [] $($rest)+)
    };
    (target: $target:expr, $level:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($crate::logger()) ($target) ($level)) [] $($rest)+)
    };
    ($level:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($crate::logger()) (::core::module_path!()) ($level)) [] $($rest)+)
    };
}

/// Logs at [`Level::Trace`](crate::Level::Trace). See [`info!`](crate::info!) for the syntax.
#[macro_export]
macro_rules! trace {
    ($($t:tt)+) => { $crate::__log_level!(($crate::Level::Trace) $($t)+) };
}

/// Logs at [`Level::Debug`](crate::Level::Debug). See [`info!`](crate::info!) for the syntax.
#[macro_export]
macro_rules! debug {
    ($($t:tt)+) => { $crate::__log_level!(($crate::Level::Debug) $($t)+) };
}

/// Logs at [`Level::Info`](crate::Level::Info).
///
/// ```text
/// info!([logger: <&Logger or Logger>,] [target: <&'static str>,] "format string" [, format args…] [; fields…])
/// ```
///
/// - The message is a `format!` string with its arguments, including inline captures
///   (`"user {name} logged in"`).
/// - Fields follow a `;` as comma-separated `key = value` pairs. The key is an
///   identifier or a string literal (`"http.status" = 200`). The value is:
///   - `value`: converted with [`ToJson`](crate::ToJson), keeping numbers, booleans,
///     strings, `Option`, `Vec` and your own `ToJson` types structured;
///   - `%value`: its `Display` output, as a string;
///   - `?value`: its `Debug` output, as a string.
///
///   `name`, `%name` and `?name` alone are shorthand for `name = name` and so on.
/// - `logger:` logs to that logger instead of the global one ([`logger()`](crate::logger)).
/// - `target:` replaces the default target, `module_path!()`.
///
/// Nothing after the level is evaluated unless the logger accepts the level and
/// target: neither the format arguments nor the field values.
///
/// ```
/// use dl_log::{Logger, MemorySink, TextFormat, info};
///
/// #[derive(Debug)]
/// enum Channel { Web }
///
/// let mem = MemorySink::new().with_format(TextFormat::new().with_time(false));
/// let log = Logger::builder().sink(mem.clone()).build();
///
/// let id = 42u64;
/// let guest = String::from("Nguyễn Văn An");
/// let channel = Channel::Web;
/// let total = dl_time::Duration::from_millis(1500);
///
/// info!(logger: log, "booking confirmed"; booking_id = id, nights = 3, guest = %guest, ?channel);
/// info!(logger: log, "user {} logged in", guest);
/// info!(logger: log, "took {total}"; "http.status" = 201, tags = vec!["vip"], coupon = None::<&str>);
///
/// assert_eq!(mem.lines(), [
///     r#"INFO  booking confirmed booking_id=42 nights=3 guest="Nguyễn Văn An" channel=Web"#,
///     "INFO  user Nguyễn Văn An logged in",
///     r#"INFO  took 1.5s http.status=201 tags=["vip"] coupon=null"#,
/// ]);
/// ```
#[macro_export]
macro_rules! info {
    ($($t:tt)+) => { $crate::__log_level!(($crate::Level::Info) $($t)+) };
}

/// Logs at [`Level::Warn`](crate::Level::Warn). See [`info!`](crate::info!) for the syntax.
#[macro_export]
macro_rules! warn {
    ($($t:tt)+) => { $crate::__log_level!(($crate::Level::Warn) $($t)+) };
}

/// Logs at [`Level::Error`](crate::Level::Error). See [`info!`](crate::info!) for the syntax.
#[macro_export]
macro_rules! error {
    ($($t:tt)+) => { $crate::__log_level!(($crate::Level::Error) $($t)+) };
}

/// Logs at [`Level::Fatal`](crate::Level::Fatal). See [`info!`](crate::info!) for the syntax. It does
/// not exit the process; call [`shutdown`](crate::shutdown) before exiting so queued
/// records are written.
#[macro_export]
macro_rules! fatal {
    ($($t:tt)+) => { $crate::__log_level!(($crate::Level::Fatal) $($t)+) };
}

/// Returns `true` when a record at the level would be written, to guard expensive
/// preparation of log data.
///
/// ```
/// use dl_log::{Level, Logger, log_enabled};
///
/// let log = Logger::builder().level(Level::Info).build();
/// assert!(log_enabled!(logger: log, Level::Warn));
/// assert!(!log_enabled!(logger: log, target: "sqlx", Level::Debug));
/// // Global logger (disabled until `dl_log::init`).
/// assert!(!log_enabled!(Level::Fatal));
/// ```
#[macro_export]
macro_rules! log_enabled {
    (logger: $logger:expr, target: $target:expr, $level:expr $(,)?) => {
        $crate::Logger::enabled(&$logger, $level, $target)
    };
    (logger: $logger:expr, $level:expr $(,)?) => {
        $crate::Logger::enabled(&$logger, $level, ::core::module_path!())
    };
    (target: $target:expr, $level:expr $(,)?) => {
        $crate::logger().enabled($level, $target)
    };
    ($level:expr $(,)?) => {
        $crate::logger().enabled($level, ::core::module_path!())
    };
}

/// Builds a `Vec<Field>` with the field syntax of [`info!`](crate::info!), for
/// [`with_context`](crate::with_context) and [`push_context`](crate::push_context).
///
/// ```
/// use dl_log::fields;
///
/// let request_id = "r-9";
/// let user_id = 7;
/// let f = fields!(request_id, user_id, "client.ip" = %"10.0.0.8", raw = ?"x");
/// let keys: Vec<&str> = f.iter().map(|(k, _)| k.as_ref()).collect();
/// assert_eq!(keys, ["request_id", "user_id", "client.ip", "raw"]);
/// assert_eq!(f[3].1, "\"x\"");
/// assert!(fields!().is_empty());
/// ```
#[macro_export]
macro_rules! fields {
    () => { $crate::__private::Vec::<$crate::Field>::new() };
    ($($rest:tt)+) => { $crate::__fields_munch!([] $($rest)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __fields_munch {
    ([$($out:tt)*]) => { $crate::__private::vec![$($out)*] };

    ([$($out:tt)*] $key:ident = % $value:expr $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key(::core::stringify!($key)), $crate::__private::display(&$value)),] $($($rest)*)?)
    };
    ([$($out:tt)*] $key:ident = ? $value:expr $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key(::core::stringify!($key)), $crate::__private::debug(&$value)),] $($($rest)*)?)
    };
    ([$($out:tt)*] $key:ident = $value:expr $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key(::core::stringify!($key)), $crate::__private::value(&$value)),] $($($rest)*)?)
    };
    ([$($out:tt)*] $key:literal = % $value:expr $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key($key), $crate::__private::display(&$value)),] $($($rest)*)?)
    };
    ([$($out:tt)*] $key:literal = ? $value:expr $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key($key), $crate::__private::debug(&$value)),] $($($rest)*)?)
    };
    ([$($out:tt)*] $key:literal = $value:expr $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key($key), $crate::__private::value(&$value)),] $($($rest)*)?)
    };
    ([$($out:tt)*] % $key:ident $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key(::core::stringify!($key)), $crate::__private::display(&$key)),] $($($rest)*)?)
    };
    ([$($out:tt)*] ? $key:ident $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key(::core::stringify!($key)), $crate::__private::debug(&$key)),] $($($rest)*)?)
    };
    ([$($out:tt)*] $key:ident $(, $($rest:tt)*)?) => {
        $crate::__fields_munch!([$($out)* ($crate::__private::key(::core::stringify!($key)), $crate::__private::value(&$key)),] $($($rest)*)?)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_level {
    (($level:expr) logger: $logger:expr, target: $target:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($logger) ($target) ($level)) [] $($rest)+)
    };
    (($level:expr) logger: $logger:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($logger) (::core::module_path!()) ($level)) [] $($rest)+)
    };
    (($level:expr) target: $target:expr, $($rest:tt)+) => {
        $crate::__log_munch!((($crate::logger()) ($target) ($level)) [] $($rest)+)
    };
    (($level:expr) $($rest:tt)+) => {
        $crate::__log_munch!((($crate::logger()) (::core::module_path!()) ($level)) [] $($rest)+)
    };
}

/// Splits the message tokens from the fields at the first top-level `;`.
#[doc(hidden)]
#[macro_export]
macro_rules! __log_munch {
    ($ctx:tt [$($msg:tt)*] ; $($fields:tt)*) => {
        $crate::__log_emit!($ctx [$($msg)*] [$($fields)*])
    };
    ($ctx:tt [$($msg:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__log_munch!($ctx [$($msg)* $next] $($rest)*)
    };
    ($ctx:tt [$($msg:tt)*]) => {
        $crate::__log_emit!($ctx [$($msg)*] [])
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_emit {
    ((($logger:expr) ($target:expr) ($level:expr)) [$fmt:literal $($args:tt)*] [$($fields:tt)*]) => {{
        let __dl_logger: &$crate::Logger = &$logger;
        let __dl_level: $crate::Level = $level;
        let __dl_target: &'static str = $target;
        if __dl_logger.enabled(__dl_level, __dl_target) {
            $crate::__private::log(
                __dl_logger,
                __dl_level,
                __dl_target,
                ::core::format_args!($fmt $($args)*),
                ::core::option::Option::Some($fmt),
                ::core::file!(),
                ::core::line!(),
                $crate::fields!($($fields)*),
            );
        }
    }};
    ((($logger:expr) ($target:expr) ($level:expr)) [$($msg:tt)+] [$($fields:tt)*]) => {{
        let __dl_logger: &$crate::Logger = &$logger;
        let __dl_level: $crate::Level = $level;
        let __dl_target: &'static str = $target;
        if __dl_logger.enabled(__dl_level, __dl_target) {
            $crate::__private::log(
                __dl_logger,
                __dl_level,
                __dl_target,
                ::core::format_args!($($msg)+),
                ::core::option::Option::None,
                ::core::file!(),
                ::core::line!(),
                $crate::fields!($($fields)*),
            );
        }
    }};
}
