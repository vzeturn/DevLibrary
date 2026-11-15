use super::*;
use crate::{json, to_string, to_string_pretty};
use alloc::format;
use alloc::string::ToString;
use std::println;

use ErrorKind::*;

/// Must-accept inputs, named after their JSONTestSuite (`y_*` / `i_*`) counterparts.
const ACCEPT: &[(&str, &str)] = &[
    ("array_arraysWithSpaces", "[[]   ]"),
    ("array_empty", "[]"),
    ("array_empty-string", "[\"\"]"),
    ("array_ending_with_newline", "[\"a\"]"),
    ("array_false", "[false]"),
    ("array_heterogeneous", "[null, 1, \"1\", {}]"),
    ("array_null", "[null]"),
    ("array_with_1_and_newline", "[1\n]"),
    ("array_with_leading_space", " [1]"),
    ("array_with_several_null", "[1,null,null,null,2]"),
    ("array_with_trailing_space", "[2] "),
    ("number", "[123e65]"),
    ("number_0e+1", "[0e+1]"),
    ("number_0e1", "[0e1]"),
    ("number_after_space", "[ 4]"),
    (
        "number_double_close_to_zero",
        "[-0.000000000000000000000000000000000000000000000000000000000000000000000000000001]",
    ),
    ("number_int_with_exp", "[20e1]"),
    ("number_minus_zero", "[-0]"),
    ("number_negative_int", "[-123]"),
    ("number_negative_one", "[-1]"),
    ("number_negative_zero", "[-0]"),
    ("number_real_capital_e", "[1E22]"),
    ("number_real_capital_e_neg_exp", "[1E-2]"),
    ("number_real_capital_e_pos_exp", "[1E+2]"),
    ("number_real_exponent", "[123e45]"),
    ("number_real_fraction_exponent", "[123.456e78]"),
    ("number_real_neg_exp", "[1e-2]"),
    ("number_real_pos_exponent", "[1e+2]"),
    ("number_simple_int", "[123]"),
    ("number_simple_real", "[123.456789]"),
    ("i_number_double_huge_neg_exp", "[123.456e-789]"),
    (
        "i_number_too_big_neg_int",
        "[-123123123123123123123123123123]",
    ),
    ("i_number_too_big_pos_int", "[100000000000000000000]"),
    (
        "i_number_very_big_negative_int",
        "[-237462374673276894279832749832423479823246327846]",
    ),
    ("object", "{\"asd\":\"sdf\", \"dfg\":\"fgh\"}"),
    ("object_basic", "{\"asd\":\"sdf\"}"),
    ("object_duplicated_key", "{\"a\":\"b\",\"a\":\"c\"}"),
    (
        "object_duplicated_key_and_value",
        "{\"a\":\"b\",\"a\":\"b\"}",
    ),
    ("object_empty", "{}"),
    ("object_empty_key", "{\"\":0}"),
    ("object_escaped_null_in_key", "{\"foo\\u0000bar\": 42}"),
    (
        "object_extreme_numbers",
        "{ \"min\": -1.0e+28, \"max\": 1.0e+28 }",
    ),
    (
        "object_long_strings",
        "{\"x\":[{\"id\": \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}], \"id\": \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}",
    ),
    ("object_simple", "{\"a\":[]}"),
    (
        "object_string_unicode",
        "{\"title\":\"\\u041f\\u043e\\u043b\\u0442\\u043e\\u0440\\u0430 \\u0417\\u0435\\u043c\\u043b\\u0435\\u043a\\u043e\\u043f\\u0430\" }",
    ),
    ("object_with_newlines", "{\n\"a\": \"b\"\n}"),
    (
        "string_1_2_3_bytes_UTF-8_sequences",
        "[\"\\u0060\\u012a\\u12AB\"]",
    ),
    ("string_accepted_surrogate_pair", "[\"\\uD801\\udc37\"]"),
    (
        "string_accepted_surrogate_pairs",
        "[\"\\ud83d\\ude39\\ud83d\\udc8d\"]",
    ),
    ("string_allowed_escapes", "[\"\\\"\\\\\\/\\b\\f\\n\\r\\t\"]"),
    ("string_backslash_and_u_escaped_zero", "[\"\\\\u0000\"]"),
    ("string_backslash_doublequotes", "[\"\\\"\"]"),
    ("string_comments", "[\"a/*b*/c/*d//e\"]"),
    ("string_double_escape_a", "[\"\\\\a\"]"),
    ("string_double_escape_n", "[\"\\\\n\"]"),
    ("string_escaped_control_character", "[\"\\u0012\"]"),
    ("string_escaped_noncharacter", "[\"\\uFFFF\"]"),
    ("string_in_array", "[\"asd\"]"),
    ("string_in_array_with_leading_space", "[ \"asd\"]"),
    ("string_last_surrogates_1_and_2", "[\"\\uDBFF\\uDFFF\"]"),
    ("string_nbsp_uescaped", "[\"new\\u00A0line\"]"),
    ("string_nonCharacterInUTF-8_U+10FFFF", "[\"\u{10FFFF}\"]"),
    ("string_null_escape", "[\"\\u0000\"]"),
    ("string_one-byte-utf-8", "[\"\\u002c\"]"),
    ("string_pi", "[\"π\"]"),
    ("string_reservedCharacterInUTF-8_U+1BFFF", "[\"\u{1BFFF}\"]"),
    ("string_simple_ascii", "[\"asd \"]"),
    ("string_space", "\" \""),
    (
        "string_surrogates_U+1D11E_MUSICAL_SYMBOL_G_CLEF",
        "[\"\\uD834\\uDd1e\"]",
    ),
    ("string_three-byte-utf-8", "[\"\\u0821\"]"),
    ("string_two-byte-utf-8", "[\"\\u0123\"]"),
    ("string_u+2028_line_sep", "[\"\u{2028}\"]"),
    ("string_u+2029_par_sep", "[\"\u{2029}\"]"),
    ("string_uEscape", "[\"\\u0061\\u30af\\u30EA\\u30b9\"]"),
    ("string_unicode", "[\"\\uA66D\"]"),
    ("string_utf8", "[\"€𝄞\"]"),
    ("string_with_del_character", "[\"a\u{7f}a\"]"),
    ("structure_lonely_false", "false"),
    ("structure_lonely_int", "42"),
    ("structure_lonely_negative_real", "-0.1"),
    ("structure_lonely_null", "null"),
    ("structure_lonely_string", "\"asd\""),
    ("structure_lonely_true", "true"),
    ("structure_string_empty", "\"\""),
    ("structure_trailing_newline", "[\"a\"]\n"),
    ("structure_true_in_array", "[true]"),
    ("structure_whitespace_array", " [] "),
    (
        "structure_all_whitespace_kinds",
        " \t\r\n[ \t\r\n1 \t\r\n, \t\r\n{ \t\r\n\"k\" \t\r\n: \t\r\n2 \t\r\n} \t\r\n] \t\r\n",
    ),
    ("i_structure_UTF-8_BOM_empty_object", "\u{feff}{}"),
];

/// Must-reject inputs with the expected error kind, named after JSONTestSuite `n_*`
/// cases where one exists. `1e400` is an `i_*` case that this crate rejects by design.
const REJECT: &[(&str, &str, ErrorKind)] = &[
    (
        "array_1_true_without_comma",
        "[1 true]",
        UnexpectedChar('t'),
    ),
    ("array_a_invalid_utf8", "[a]", UnexpectedChar('a')),
    (
        "array_colon_instead_of_comma",
        "[\"\": 1]",
        UnexpectedChar(':'),
    ),
    ("array_comma_after_close", "[\"\"],", TrailingCharacters),
    ("array_comma_and_number", "[,1]", UnexpectedChar(',')),
    ("array_double_comma", "[1,,2]", UnexpectedChar(',')),
    ("array_double_extra_comma", "[\"x\",,]", UnexpectedChar(',')),
    ("array_extra_close", "[\"x\"]]", TrailingCharacters),
    ("array_extra_comma", "[\"\",]", UnexpectedChar(']')),
    ("array_incomplete", "[\"x\"", UnexpectedEof),
    ("array_incomplete_invalid_value", "[x", UnexpectedChar('x')),
    ("array_inner_array_no_comma", "[3[4]]", UnexpectedChar('[')),
    (
        "array_items_separated_by_semicolon",
        "[1:2]",
        UnexpectedChar(':'),
    ),
    ("array_just_comma", "[,]", UnexpectedChar(',')),
    ("array_just_minus", "[-]", InvalidNumber),
    ("array_missing_value", "[   , \"\"]", UnexpectedChar(',')),
    ("array_newlines_unclosed", "[\"a\",\n4\n,1,", UnexpectedEof),
    ("array_number_and_comma", "[1,]", UnexpectedChar(']')),
    (
        "array_number_and_several_commas",
        "[1,,]",
        UnexpectedChar(','),
    ),
    ("array_star_inside", "[*]", UnexpectedChar('*')),
    ("array_unclosed", "[\"\"", UnexpectedEof),
    ("array_unclosed_trailing_comma", "[1,", UnexpectedEof),
    ("array_unclosed_with_object_inside", "[{}", UnexpectedEof),
    ("incomplete_false", "[fals]", UnexpectedChar(']')),
    ("incomplete_null", "[nul]", UnexpectedChar(']')),
    ("incomplete_true", "[tru]", UnexpectedChar(']')),
    ("number_++", "[++1234]", UnexpectedChar('+')),
    ("number_+1", "[+1]", UnexpectedChar('+')),
    ("number_+Inf", "[+Inf]", UnexpectedChar('+')),
    ("number_-01", "[-01]", InvalidNumber),
    ("number_-1.0.", "[-1.0.]", UnexpectedChar('.')),
    ("number_-2.", "[-2.]", InvalidNumber),
    ("number_-NaN", "[-NaN]", InvalidNumber),
    ("number_.-1", "[.-1]", UnexpectedChar('.')),
    ("number_.2e-3", "[.2e-3]", UnexpectedChar('.')),
    ("number_0.1.2", "[0.1.2]", UnexpectedChar('.')),
    ("number_0.3e+", "[0.3e+]", InvalidNumber),
    ("number_0.3e", "[0.3e]", InvalidNumber),
    ("number_0.e1", "[0.e1]", InvalidNumber),
    ("number_0_capital_E+", "[0E+]", InvalidNumber),
    ("number_0e", "[0e]", InvalidNumber),
    ("number_1.0e-", "[1.0e-]", InvalidNumber),
    ("number_1_000", "[1 000.0]", UnexpectedChar('0')),
    ("number_1eE2", "[1eE2]", InvalidNumber),
    ("number_2.e+3", "[2.e+3]", InvalidNumber),
    ("number_9.e+", "[9.e+]", InvalidNumber),
    ("number_Inf", "[Inf]", UnexpectedChar('I')),
    ("number_NaN", "[NaN]", UnexpectedChar('N')),
    ("number_infinity", "[Infinity]", UnexpectedChar('I')),
    ("number_minus_infinity", "[-Infinity]", InvalidNumber),
    ("number_hex_1_digit", "[0x1]", UnexpectedChar('x')),
    ("number_with_leading_zero", "[012]", InvalidNumber),
    (
        "number_neg_with_garbage_at_end",
        "[-1x]",
        UnexpectedChar('x'),
    ),
    ("number_minus_space_1", "[- 1]", InvalidNumber),
    ("number_real_without_fractional_part", "[1.]", InvalidNumber),
    ("number_plus_inside", "[1+2]", UnexpectedChar('+')),
    ("number_exp_sign_twice", "[0e+-1]", InvalidNumber),
    ("i_number_huge_exp", "[1e400]", InvalidNumber),
    ("i_number_neg_huge_exp", "[-1.5e+9999]", InvalidNumber),
    ("number_just_minus_top_level", "-", UnexpectedEof),
    ("object_missing_colon", "{\"a\" b}", UnexpectedChar('b')),
    (
        "object_missing_semicolon",
        "{\"a\" \"b\"}",
        UnexpectedChar('"'),
    ),
    ("object_missing_key", "{:\"b\"}", UnexpectedChar(':')),
    ("object_trailing_comma", "{\"id\":0,}", UnexpectedChar('}')),
    (
        "object_garbage_at_end",
        "{\"a\":\"a\" 123}",
        UnexpectedChar('1'),
    ),
    ("object_unquoted_key", "{a: \"b\"}", UnexpectedChar('a')),
    ("object_single_quote", "{'a':0}", UnexpectedChar('\'')),
    ("object_non_string_key", "{1:1}", UnexpectedChar('1')),
    (
        "object_repeated_null_null",
        "{null:null,null:null}",
        UnexpectedChar('n'),
    ),
    (
        "object_trailing_comment",
        "{\"a\":\"b\"}/**/",
        TrailingCharacters,
    ),
    (
        "object_trailing_comment_slash_open",
        "{\"a\":\"b\"}//",
        TrailingCharacters,
    ),
    (
        "object_with_comment",
        "{\"a\":/*comment*/\"b\"}",
        UnexpectedChar('/'),
    ),
    (
        "object_two_commas_in_a_row",
        "{\"a\":\"b\",,\"c\":\"d\"}",
        UnexpectedChar(','),
    ),
    ("object_unterminated-value", "{\"a\":\"a", UnexpectedEof),
    ("object_no-colon", "{\"a\"", UnexpectedEof),
    ("object_missing_value", "{\"a\":", UnexpectedEof),
    (
        "object_several_trailing_commas",
        "{\"id\":0,,,,,}",
        UnexpectedChar(','),
    ),
    (
        "object_with_trailing_garbage",
        "{\"a\": true} \"x\"",
        TrailingCharacters,
    ),
    (
        "object_comma_instead_of_colon",
        "{\"x\", null}",
        UnexpectedChar(','),
    ),
    (
        "object_lone_continuation_key",
        "{\"\\uDFAA\":0}",
        InvalidUnicode,
    ),
    (
        "string_1_surrogate_then_escape",
        "[\"\\uD800\\\"]",
        InvalidUnicode,
    ),
    (
        "string_1_surrogate_then_escape_u",
        "[\"\\uD800\\u\"]",
        InvalidUnicode,
    ),
    (
        "string_1_surrogate_then_escape_u1",
        "[\"\\uD800\\u1\"]",
        InvalidUnicode,
    ),
    (
        "string_1_surrogate_then_escape_u1x",
        "[\"\\uD800\\u1x\"]",
        InvalidUnicode,
    ),
    (
        "string_incomplete_surrogate_pair",
        "[\"\\uDd1ea\"]",
        InvalidUnicode,
    ),
    (
        "string_lone_high_surrogate",
        "[\"\\ud800\"]",
        InvalidUnicode,
    ),
    (
        "string_high_surrogate_then_text",
        "[\"\\ud800abc\"]",
        InvalidUnicode,
    ),
    (
        "string_high_then_high_surrogate",
        "[\"\\uD800\\uD800\"]",
        InvalidUnicode,
    ),
    ("string_escape_x", "[\"\\x00\"]", InvalidEscape),
    (
        "string_escaped_backslash_bad",
        "[\"\\\\\\\"]",
        UnexpectedEof,
    ),
    ("string_escaped_ctrl_char_tab", "[\"\\\t\"]", InvalidEscape),
    ("string_escaped_emoji", "[\"\\🌀\"]", InvalidEscape),
    ("string_incomplete_escape", "[\"\\\"]", UnexpectedEof),
    (
        "string_incomplete_escaped_character",
        "[\"\\u00A\"]",
        InvalidUnicode,
    ),
    (
        "string_invalid_unicode_escape",
        "[\"\\uqqqq\"]",
        InvalidUnicode,
    ),
    ("string_invalid_backslash_esc", "[\"\\a\"]", InvalidEscape),
    ("string_escaped_single_quote", "[\"\\'\"]", InvalidEscape),
    (
        "string_single_quote",
        "['single quote']",
        UnexpectedChar('\''),
    ),
    (
        "string_no_quotes_with_bad_escape",
        "[\\n]",
        UnexpectedChar('\\'),
    ),
    ("string_unescaped_tab", "[\"a\ta\"]", ControlCharInString),
    (
        "string_unescaped_newline",
        "[\"new\nline\"]",
        ControlCharInString,
    ),
    (
        "string_unescaped_ctrl_char",
        "[\"a\u{0}a\"]",
        ControlCharInString,
    ),
    (
        "string_unescaped_unit_separator",
        "[\"\u{1f}\"]",
        ControlCharInString,
    ),
    ("string_single_doublequote", "\"", UnexpectedEof),
    ("string_start_escape_unclosed", "[\"\\", UnexpectedEof),
    ("string_truncated_unicode", "\"\\u12", UnexpectedEof),
    (
        "structure_whitespace_U+2060_word_joiner",
        "[\u{2060}]",
        UnexpectedChar('\u{2060}'),
    ),
    (
        "structure_whitespace_formfeed",
        "[\u{c}]",
        UnexpectedChar('\u{c}'),
    ),
    (
        "structure_nbsp_as_whitespace",
        "[\u{a0}1]",
        UnexpectedChar('\u{a0}'),
    ),
    (
        "structure_vertical_tab",
        "[\u{b}1]",
        UnexpectedChar('\u{b}'),
    ),
    ("structure_UTF8_BOM_no_data", "\u{feff}", UnexpectedEof),
    ("structure_no_data", "", UnexpectedEof),
    ("structure_whitespace_only", " \n\t ", UnexpectedEof),
    (
        "structure_array_trailing_garbage",
        "[1]x",
        TrailingCharacters,
    ),
    ("structure_close_unopened_array", "]", UnexpectedChar(']')),
    ("structure_lone-open-bracket", "[", UnexpectedEof),
    (
        "structure_object_followed_by_closing_object",
        "{}}",
        TrailingCharacters,
    ),
    (
        "structure_number_with_trailing_garbage",
        "2@",
        TrailingCharacters,
    ),
    ("structure_two_values", "2 3", TrailingCharacters),
    ("structure_capitalized_True", "[True]", UnexpectedChar('T')),
    (
        "structure_ascii-unicode-identifier",
        "aå",
        UnexpectedChar('a'),
    ),
    ("structure_unicode-identifier", "å", UnexpectedChar('å')),
    ("structure_null_truncated", "nul", UnexpectedEof),
    ("structure_true_with_garbage", "truex", TrailingCharacters),
    (
        "structure_angle_bracket_null",
        "[<null>]",
        UnexpectedChar('<'),
    ),
    (
        "structure_U+2028_after_value",
        "[1]\u{2028}",
        TrailingCharacters,
    ),
    ("structure_double_array", "[][]", TrailingCharacters),
];

#[test]
fn accept_table() {
    assert!(ACCEPT.len() >= 40, "{}", ACCEPT.len());
    for (name, src) in ACCEPT {
        let v = parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        // Every accepted document survives a serialize/parse round trip.
        let compact = to_string(&v);
        assert_eq!(parse(&compact).unwrap(), v, "{name}");
        assert_eq!(parse(&to_string_pretty(&v)).unwrap(), v, "{name}");
        assert_eq!(parse_slice(src.as_bytes()).unwrap(), v, "{name}");
    }
}

#[test]
fn reject_table() {
    assert!(REJECT.len() >= 50, "{}", REJECT.len());
    for (name, src, kind) in REJECT {
        match parse(src) {
            Ok(v) => panic!("{name}: accepted as {v}"),
            Err(e) => assert_eq!(e.kind(), kind, "{name}: {e}"),
        }
        assert!(parse_slice(src.as_bytes()).is_err(), "{name}");
    }
    println!("{} accept, {} reject cases", ACCEPT.len(), REJECT.len());
}

#[test]
fn accepted_values() {
    let v = parse("{\"a\":\"b\",\"a\":\"c\",\"z\":1}").unwrap();
    assert_eq!(v.to_string(), r#"{"a":"c","z":1}"#);
    assert_eq!(
        parse("[\"\\\"\\\\\\/\\b\\f\\n\\r\\t\"]").unwrap()[0],
        "\"\\/\u{8}\u{c}\n\r\t"
    );
    assert_eq!(parse("{\"foo\\u0000bar\": 42}").unwrap()["foo\0bar"], 42);
    assert_eq!(parse("[\"\\\\u0000\"]").unwrap()[0], "\\u0000");
    assert_eq!(parse("[123.456e-789]").unwrap()[0], 0.0);
    assert_eq!(parse("\u{feff}{}").unwrap(), json!({}));
    assert_eq!(parse("[\"a\u{7f}a\"]").unwrap()[0], "a\u{7f}a");
}

#[test]
fn rfc8259_examples() {
    // RFC 8259 section 13.
    let image = parse(
        r#"{
      "Image": {
          "Width":  800,
          "Height": 600,
          "Title":  "View from 15th Floor",
          "Thumbnail": {
              "Url":    "http://www.example.com/image/481989943",
              "Height": 125,
              "Width":  100
          },
          "Animated" : false,
          "IDs": [116, 943, 234, 38793]
        }
    }"#,
    )
    .unwrap();
    assert_eq!(image["Image"]["Width"], 800);
    assert_eq!(image["Image"]["Title"], "View from 15th Floor");
    assert_eq!(
        image["Image"]["Thumbnail"]["Url"],
        "http://www.example.com/image/481989943"
    );
    assert_eq!(image["Image"]["Animated"], false);
    assert_eq!(image["Image"]["IDs"], json!([116, 943, 234, 38793]));
    assert_eq!(
        image["Image"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["Width", "Height", "Title", "Thumbnail", "Animated", "IDs"]
    );

    let places = parse(
        r#"[
      {
         "precision": "zip",
         "Latitude":  37.7668,
         "Longitude": -122.3959,
         "Address":   "",
         "City":      "SAN FRANCISCO",
         "State":     "CA",
         "Zip":       "94107",
         "Country":   "US"
      },
      {
         "precision": "zip",
         "Latitude":  37.371991,
         "Longitude": -122.026020,
         "Address":   "",
         "City":      "SUNNYVALE",
         "State":     "CA",
         "Zip":       "94085",
         "Country":   "US"
      }
    ]"#,
    )
    .unwrap();
    assert_eq!(places[0]["Latitude"], 37.7668);
    assert_eq!(
        places[1]["Longitude"].as_f64().unwrap().to_bits(),
        0xc05e81aa4fca42af
    );
    assert_eq!(places[1]["Zip"], "94085");
    assert_eq!(places.to_string().matches("\"Country\":\"US\"").count(), 2);
    // Section 13 also shows top-level scalars.
    assert_eq!(parse("\"Hello world!\"").unwrap(), "Hello world!");
    assert_eq!(parse("42").unwrap(), 42);
    assert_eq!(parse("true").unwrap(), true);
    // Section 7: G clef as a surrogate pair.
    assert_eq!(parse("\"\\uD834\\uDD1E\"").unwrap(), "\u{1D11E}");
}

#[test]
fn unicode() {
    assert_eq!(parse("\"\\ud83d\\ude00\"").unwrap(), "😀");
    assert_eq!(parse("\"\\u00e9\\u00C9\"").unwrap(), "éÉ");
    assert_eq!(parse("\"Đà Nẵng 🏖 ️\"").unwrap(), "Đà Nẵng 🏖 ️");
    assert_eq!(parse("\"\\u2028\\u2029\"").unwrap(), "\u{2028}\u{2029}");
    assert_eq!(parse("\"\\uDBFF\\uDFFF\"").unwrap(), "\u{10FFFF}");
    assert_eq!(parse("\"\\uD800\\uDC00\"").unwrap(), "\u{10000}");
    for lone in [
        "\"\\ud83d\"",
        "\"\\ude00\"",
        "\"\\ude00\\ud83d\"",
        "\"\\ud83d\\u0041\"",
    ] {
        let e = parse(lone).unwrap_err();
        assert_eq!((e.kind(), e.offset()), (&InvalidUnicode, 1), "{lone}");
    }
    // Escapes and plain runs at every offset across the 8-byte scanner boundary.
    for pad in 0..18 {
        let p = "x".repeat(pad);
        let src = format!("\"{p}\\n{p}\\u00e9{p}é{p}\"");
        let expected = format!("{p}\n{p}é{p}é{p}");
        assert_eq!(parse(&src).unwrap(), expected.as_str());
        let bad = format!("\"{p}\u{1}\"");
        assert_eq!(parse(&bad).unwrap_err().offset(), pad + 1);
    }
}

#[test]
fn invalid_utf8() {
    let cases: &[(&[u8], usize)] = &[
        (b"[\"a\xffb\"]", 3),
        (b"\"\xc0\xaf\"", 1),         // overlong encoding of '/'
        (b"\"\xed\xa0\x80\"", 1),     // UTF-8 encoded surrogate U+D800
        (b"[\"\xe2\x82\"]", 2),       // truncated 3-byte sequence
        (b"\"\x80\"", 1),             // lone continuation byte
        (b"\"\xf4\x90\x80\x80\"", 1), // above U+10FFFF
        (b"\xef\xbb", 0),             // partial BOM
    ];
    for &(src, offset) in cases {
        let e = parse_slice(src).unwrap_err();
        assert_eq!(
            (e.kind(), e.offset(), e.column()),
            (&InvalidUtf8, offset, offset + 1)
        );
    }
    assert_eq!(parse_slice(b"\xef\xbb\xbf[1]").unwrap(), json!([1]));
    assert_eq!(parse_slice("\"Hội An\"".as_bytes()).unwrap(), "Hội An");
}

#[test]
fn error_positions() {
    let e = parse("{\n\"a\": [\n  1, 2x\n]}").unwrap_err();
    assert_eq!(e.to_string(), "unexpected character 'x' at line 3 column 7");
    assert_eq!((e.line(), e.column(), e.offset()), (3, 7, 15));

    let e = parse("{\"tên\": \"Hà Nội\", x}").unwrap_err();
    assert_eq!((e.line(), e.column(), e.offset()), (1, 19, 22));

    let e = parse("{\n  \"a\": 1,\n  \"b\": tru\n}").unwrap_err();
    assert_eq!(
        e.to_string(),
        "unexpected character '\\n' at line 3 column 11"
    );

    let e = parse("\u{feff}[x]").unwrap_err();
    assert_eq!((e.offset(), e.line(), e.column()), (4, 1, 2));

    let e = parse("[1,\r\n2,\r\n").unwrap_err();
    assert_eq!(
        (e.kind(), e.line(), e.column(), e.offset()),
        (&UnexpectedEof, 3, 1, 9)
    );
    assert_eq!(e.to_string(), "unexpected end of input at line 3 column 1");

    let e = parse("[1e400]").unwrap_err();
    assert_eq!((e.kind(), e.offset()), (&InvalidNumber, 1));
    let e = parse("[01]").unwrap_err();
    assert_eq!((e.kind(), e.offset()), (&InvalidNumber, 2));
    let e = parse("\"ab\\qcd\"").unwrap_err();
    assert_eq!((e.kind(), e.offset()), (&InvalidEscape, 3));
    let e = parse("[\"x\"] ,").unwrap_err();
    assert_eq!((e.kind(), e.offset()), (&TrailingCharacters, 6));
    assert_eq!(e.to_string(), "trailing characters at line 1 column 7");
}

fn num(src: &str) -> Number {
    parse(src).unwrap().as_number().unwrap().clone()
}

#[test]
fn numbers() {
    let z = num("-0");
    assert!(z.is_f64() && z.as_f64().unwrap().is_sign_negative());
    assert_eq!(to_string(&Value::Number(z.clone())), "-0");
    assert_eq!(z, num("0"));
    assert_eq!(num("0").as_u64(), Some(0));
    assert_eq!(num("-9223372036854775808").as_i64(), Some(i64::MIN));
    assert_eq!(num("-9223372036854775807").as_i64(), Some(i64::MIN + 1));
    // One below i64::MIN: falls back to f64 (Python: float(...).hex() == c3e0...).
    let n = num("-9223372036854775809");
    assert!(n.is_f64() && n.as_i64().is_none());
    assert_eq!(n.as_f64().unwrap().to_bits(), 0xc3e0000000000000);
    assert_eq!(num("18446744073709551615").as_u64(), Some(u64::MAX));
    assert_eq!(
        num("10000000000000000000").as_u64(),
        Some(10_000_000_000_000_000_000)
    );
    assert_eq!(
        num("9999999999999999999").as_u64(),
        Some(9_999_999_999_999_999_999)
    );
    assert_eq!(
        num("-9999999999999999999").as_f64().unwrap(),
        -9999999999999999999.0
    );
    let n = num("18446744073709551616");
    assert!(n.is_f64() && n.as_u64().is_none());
    assert_eq!(n.as_f64().unwrap().to_bits(), 0x43f0000000000000);
    assert_eq!(num("5e-324").as_f64().unwrap().to_bits(), 1);
    assert_eq!(
        num("2.4703282292062328e-324").as_f64().unwrap().to_bits(),
        1
    );
    assert_eq!(num("2.4703282292062327e-324").as_f64().unwrap(), 0.0);
    assert_eq!(num("1.7976931348623158e308").as_f64().unwrap(), f64::MAX);
    assert_eq!(
        parse("1.7976931348623159e308").unwrap_err().kind(),
        &InvalidNumber
    );
    assert_eq!(parse("1e400").unwrap_err().kind(), &InvalidNumber);
    assert_eq!(parse("-1e400").unwrap_err().kind(), &InvalidNumber);
    assert_eq!(num("1e-400").as_f64(), Some(0.0));
    assert_eq!(
        num("0.1000000000000000055511151231257827").as_f64(),
        Some(0.1)
    );
    assert_eq!(
        num("-237462374673276894279832749832423479823246327846")
            .as_f64()
            .unwrap()
            .to_bits(),
        0xc9c4cc172ff39c42
    );
    assert_eq!(num("1E22").as_f64().unwrap().to_bits(), 0x4480f0cf064dd592);
    assert_eq!(
        num("123.456e78").as_f64().unwrap().to_bits(),
        0x5090a8bf4f16c2a7
    );
    assert!(num("1.0").is_f64() && num("1.0") == num("1"));
    assert!(num("1e2").is_f64() && num("1e2") == num("100"));
}

#[test]
fn fast_path_matches_core_parser() {
    // Every shape the one-pass converter handles is cross-checked against core's
    // correctly rounded `str::parse::<f64>`.
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    for _ in 0..50_000 {
        let bits = rng.below(54) + 1;
        let int = rng.below(1 << bits);
        let frac_len = rng.below(8) as usize;
        let frac: String = (0..frac_len)
            .map(|_| char::from(b'0' + rng.below(10) as u8))
            .collect();
        let mut s = format!("{}{int}", if rng.next() & 1 == 0 { "-" } else { "" });
        if frac_len > 0 {
            s.push('.');
            s.push_str(&frac);
        }
        if rng.below(3) == 0 {
            s.push_str(&format!("e{}", rng.below(60) as i64 - 30));
        }
        let expected: f64 = s.parse().unwrap();
        let got = num(&s);
        if s.contains(['.', 'e']) {
            assert_eq!(got.as_f64().unwrap().to_bits(), expected.to_bits(), "{s}");
        } else {
            assert_eq!(got.as_f64().unwrap(), expected, "{s}");
        }
    }
    for s in [
        "0.1",
        "-0.0",
        "1e22",
        "9007199254740993e-22",
        "123456.789e3",
        "4.35",
        "1E-22",
    ] {
        assert_eq!(
            num(s).as_f64().unwrap().to_bits(),
            s.parse::<f64>().unwrap().to_bits(),
            "{s}"
        );
    }
}

#[test]
fn raw_numbers() {
    let opts = ParseOptions {
        keep_raw_numbers: true,
        ..ParseOptions::default()
    };
    let src = "[0,-0,10.50,1E+2,-1.25e-3,18446744073709551616,1e400,99999999999999999999.99]";
    let v = parse_with(src, opts).unwrap();
    assert_eq!(to_string(&v), src);
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|n| n.as_number().unwrap().is_raw())
    );
    assert_eq!(v[2].as_number().unwrap().as_raw_str(), Some("10.50"));
    assert_eq!(v[2], 10.5);
    assert_eq!(v[0].as_u64(), Some(0));
    assert_eq!(v[6].as_f64(), None);
    // Raw and cooked parses agree numerically where the cooked one exists.
    let cooked = parse("[0,-0,10.50,1E+2,-1.25e-3,18446744073709551616]").unwrap();
    for (i, c) in cooked.as_array().unwrap().iter().enumerate() {
        assert_eq!(&v[i], c, "{i}");
    }
    // Grammar is still enforced.
    for bad in ["[01]", "[1.]", "[-]", "[+1]", "[.5]"] {
        assert!(parse_with(bad, opts).is_err(), "{bad}");
    }
    assert_eq!(
        parse_slice_with(b"[1.10]", opts).unwrap().to_string(),
        "[1.10]"
    );
}

#[test]
fn depth_limit() {
    let nested = |n: usize| "[".repeat(n) + &"]".repeat(n);
    assert!(parse(&nested(128)).is_ok());
    let e = parse(&nested(129)).unwrap_err();
    assert_eq!((e.kind(), e.offset()), (&DepthLimitExceeded, 128));
    let objects = "{\"a\":".repeat(129) + "1" + &"}".repeat(129);
    assert_eq!(parse(&objects).unwrap_err().kind(), &DepthLimitExceeded);
    // Hostile input fails fast instead of overflowing the stack.
    assert_eq!(
        parse(&"[".repeat(1_000_000)).unwrap_err().kind(),
        &DepthLimitExceeded
    );
    let zero = ParseOptions {
        max_depth: 0,
        ..ParseOptions::default()
    };
    assert_eq!(parse_with("1", zero).unwrap(), 1);
    assert_eq!(
        parse_with("[]", zero).unwrap_err().kind(),
        &DepthLimitExceeded
    );
    let two = ParseOptions {
        max_depth: 2,
        ..ParseOptions::default()
    };
    assert!(parse_with("[{}]", two).is_ok());
    assert!(parse_with("[{\"a\":[]}]", two).is_err());
}

/// Deterministic generator for round-trip tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn string(&mut self) -> String {
        const POOL: &[char] = &[
            'a',
            'Z',
            '0',
            ' ',
            '"',
            '\\',
            '/',
            '\n',
            '\t',
            '\u{0}',
            '\u{1f}',
            '\u{7f}',
            'é',
            'ộ',
            '€',
            '\u{2028}',
            '\u{2029}',
            '\u{feff}',
            '😀',
            '\u{10ffff}',
            '“',
        ];
        let len = self.below(24) as usize;
        (0..len)
            .map(|_| POOL[self.below(POOL.len() as u64) as usize])
            .collect()
    }

    fn value(&mut self, depth: usize) -> Value {
        let pick = if depth > 4 {
            self.below(6)
        } else {
            self.below(9)
        };
        match pick {
            0 => Value::Null,
            1 => Value::Bool(self.next() & 1 == 0),
            2 => Value::from(self.next()),
            3 => Value::from(self.next() as i64),
            4 => Value::from(f64::from_bits(self.next())), // non-finite -> null
            5 => Value::String(self.string()),
            6 | 7 => Value::Array((0..self.below(6)).map(|_| self.value(depth + 1)).collect()),
            _ => Value::Object(
                (0..self.below(12))
                    .map(|_| (self.string(), self.value(depth + 1)))
                    .collect(),
            ),
        }
    }
}

#[test]
fn round_trip_random_documents() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..500 {
        let v = rng.value(0);
        let s = to_string(&v);
        let back = parse(&s).unwrap_or_else(|e| panic!("{e}: {s}"));
        assert_eq!(back, v, "{s}");
        assert_eq!(to_string(&back), s);
        assert_eq!(parse(&to_string_pretty(&v)).unwrap(), v);
        assert_eq!(parse_slice(s.as_bytes()).unwrap(), v);
    }
}

#[test]
fn every_prefix_of_a_document_is_rejected_without_panic() {
    let doc = r#"{"id":"BK-1","guest":{"name":"Phạm \"Minh\"","vip":true},"rooms":[101,-2.5e-3,null],"note":"\ud83d\ude00\n"}"#;
    assert!(parse(doc).is_ok());
    for end in 0..doc.len() {
        if let Some(prefix) = doc.get(..end) {
            assert!(parse(prefix).is_err(), "{prefix}");
        }
    }
    // Arbitrary bytes never panic.
    let mut rng = Rng(7);
    for _ in 0..2000 {
        let len = rng.below(16) as usize;
        const POOL: &[u8] = b"[]{}\":,.-+eE0129tfnul\\ \x01\xc3\xa9";
        let bytes: Vec<u8> = (0..len)
            .map(|_| POOL[rng.below(POOL.len() as u64) as usize])
            .collect();
        let _ = parse_slice(&bytes);
    }
}
