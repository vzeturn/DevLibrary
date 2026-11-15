//! The `json!` macro.

/// Builds a [`Value`](crate::Value) from JSON-like syntax.
///
/// Objects, arrays, `null`, `true` and `false` are written as in JSON. Any other value
/// is a Rust expression converted by reference with [`ToJson`](crate::ToJson), so
/// variables, fields, method calls and arithmetic all work and nothing is moved.
/// Keys are string literals or any expression whose value converts `Into<String>`;
/// wrap complex key expressions in parentheses. Trailing commas are allowed.
///
/// ```
/// use dl_json::json;
///
/// let guest = String::from("Lê Văn C");
/// let nights = 3;
/// let rate = 1_250_000;
/// let booking = json!({
///     "id": 1,
///     "guest": guest,
///     "tags": ["a", "b"],
///     "nested": {"ok": true, "n": null},
///     "total": nights * rate,
///     (format!("night_{}", nights)): [nights - 1, -1.5],
/// });
/// assert_eq!(booking["total"], 3_750_000);
/// assert_eq!(booking["nested"]["ok"], true);
/// assert_eq!(booking["night_3"][1], -1.5);
/// assert_eq!(guest, "Lê Văn C"); // still usable: values are taken by reference
/// ```
///
/// Limits: the macro recurses once per token, so very large literals may need a higher
/// `#![recursion_limit]`. A value expression ends at the next top-level comma, so a
/// comma-containing expression such as a turbofish with two type parameters must be
/// wrapped in parentheses.
#[macro_export]
macro_rules! json {
    ($($json:tt)+) => {
        $crate::__json_internal!($($json)+)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __json_internal {
    // ---- arrays: @array [finished elements,] remaining tokens ----

    // Done, with or without a trailing comma.
    (@array [$($elems:expr,)*]) => {
        $crate::__private::vec![$($elems,)*]
    };
    (@array [$($elems:expr),*]) => {
        $crate::__private::vec![$($elems),*]
    };
    // Next element is `null`, `true`, `false`, an array or an object.
    (@array [$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!(null)] $($rest)*)
    };
    (@array [$($elems:expr,)*] true $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!(true)] $($rest)*)
    };
    (@array [$($elems:expr,)*] false $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!(false)] $($rest)*)
    };
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!([$($array)*])] $($rest)*)
    };
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!({$($map)*})] $($rest)*)
    };
    // Next element is an expression followed by a comma.
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!($next),] $($rest)*)
    };
    // Last element is an expression without a trailing comma.
    (@array [$($elems:expr,)*] $last:expr) => {
        $crate::__json_internal!(@array [$($elems,)* $crate::__json_internal!($last)])
    };
    // Comma after the most recent element.
    (@array [$($elems:expr),*] , $($rest:tt)*) => {
        $crate::__json_internal!(@array [$($elems,)*] $($rest)*)
    };
    // Anything else is an error, reported at the offending token.
    (@array [$($elems:expr),*] $unexpected:tt $($rest:tt)*) => {
        $crate::__json_unexpected!($unexpected)
    };

    // ---- objects: @object map (key tokens) (remaining) (copy of remaining) ----

    // Done.
    (@object $object:ident () () ()) => {};
    // Insert the current entry, then continue after the comma.
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        let _ = $object.insert(($($key)+), $value);
        $crate::__json_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    // Current entry followed by an unexpected token.
    (@object $object:ident [$($key:tt)+] ($value:expr) $unexpected:tt $($rest:tt)*) => {
        $crate::__json_unexpected!($unexpected);
    };
    // Insert the last entry (no trailing comma).
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        let _ = $object.insert(($($key)+), $value);
    };
    // Next value is `null`, `true`, `false`, an array or an object.
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!(null)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!(true)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!(false)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!([$($array)*])) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!({$($map)*})) $($rest)*);
    };
    // Next value is an expression followed by a comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!($value)) , $($rest)*);
    };
    // Last value is an expression without a trailing comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::__json_internal!(@object $object [$($key)+] ($crate::__json_internal!($value)));
    };
    // Missing value after the colon, or missing colon: reported as "unexpected end".
    (@object $object:ident ($($key:tt)+) (:) $copy:tt) => {
        $crate::__json_internal!();
    };
    (@object $object:ident ($($key:tt)+) () $copy:tt) => {
        $crate::__json_internal!();
    };
    // A colon with no key before it.
    (@object $object:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        $crate::__json_unexpected!($colon);
    };
    // A comma inside a key.
    (@object $object:ident ($($key:tt)*) (, $($rest:tt)*) ($comma:tt $($copy:tt)*)) => {
        $crate::__json_unexpected!($comma);
    };
    // A fully parenthesized key expression.
    (@object $object:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object ($key) (: $($rest)*) (: $($rest)*));
    };
    // Something that is not an expression after the colon.
    (@object $object:ident ($($key:tt)*) (: $($unexpected:tt)+) $copy:tt) => {
        $crate::__json_expect_expr_comma!($($unexpected)+);
    };
    // Munch one token into the current key.
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::__json_internal!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    // ---- entry points ----

    (null) => {
        $crate::Value::Null
    };
    (true) => {
        $crate::Value::Bool(true)
    };
    (false) => {
        $crate::Value::Bool(false)
    };
    ([]) => {
        $crate::Value::Array($crate::__private::Vec::new())
    };
    ([ $($tt:tt)+ ]) => {
        $crate::Value::Array($crate::__json_internal!(@array [] $($tt)+))
    };
    ({}) => {
        $crate::Value::Object($crate::Map::new())
    };
    ({ $($tt:tt)+ }) => {
        $crate::Value::Object({
            let mut object = $crate::Map::new();
            $crate::__json_internal!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };
    // Any other expression: converted by reference with `ToJson`.
    ($other:expr) => {
        $crate::ToJson::to_json(&$other)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __json_unexpected {
    () => {};
}

#[macro_export]
#[doc(hidden)]
macro_rules! __json_expect_expr_comma {
    ($e:expr , $($tt:tt)*) => {};
}

#[cfg(test)]
mod tests {
    use crate::{Map, Value, parse};
    use alloc::string::{String, ToString};
    use alloc::vec;

    #[test]
    fn spec_example() {
        let some_var = vec![1.5, 2.0];
        let v =
            json!({"id": 1, "tags": ["a", "b"], "nested": {"ok": true, "n": null}, "x": some_var});
        let expected = parse(
            r#"{"id": 1, "tags": ["a", "b"], "nested": {"ok": true, "n": null}, "x": [1.5, 2]}"#,
        )
        .unwrap();
        assert_eq!(v, expected);
        assert_eq!(
            v.to_string(),
            r#"{"id":1,"tags":["a","b"],"nested":{"ok":true,"n":null},"x":[1.5,2]}"#
        );
        assert_eq!(some_var.len(), 2);
    }

    #[test]
    fn scalars_and_empty() {
        assert_eq!(json!(null), Value::Null);
        assert_eq!(json!(true), Value::Bool(true));
        assert_eq!(json!(false), Value::Bool(false));
        assert_eq!(json!([]), Value::Array(vec![]));
        assert_eq!(json!({}), Value::Object(Map::new()));
        assert_eq!(json!(-7), Value::from(-7));
        assert_eq!(json!("s"), "s");
        assert_eq!(json!(1 + 2), 3);
        assert_eq!(json!(f64::NAN), Value::Null);
    }

    #[test]
    fn nesting_commas_and_keys() {
        let key = String::from("dyn");
        let nested = json!({"in": [1]});
        let v = json!({
            "a": [null, true, false, [], {}, [[1, 2], {"b": null}],],
            (key.as_str()): nested,
            "neg": -1,
            "opt": Some(5),
            "none": None::<u8>,
            "expr": if key.is_empty() { 0 } else { 1 },
        });
        assert_eq!(
            v.to_string(),
            r#"{"a":[null,true,false,[],{},[[1,2],{"b":null}]],"dyn":{"in":[1]},"neg":-1,"opt":5,"none":null,"expr":1}"#
        );
        assert_eq!(nested["in"][0], 1);
        let arr = json!([1, "two", 3.5, {"k": [true]}]);
        assert_eq!(arr.to_string(), r#"[1,"two",3.5,{"k":[true]}]"#);
        // Duplicate keys behave like Map::insert.
        assert_eq!(json!({"k": 1, "k": 2}).to_string(), r#"{"k":2}"#);
    }
}
