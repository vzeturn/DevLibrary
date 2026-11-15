//! `cargo bench -p dl-json`
use dl_bench::Bench;
use dl_json::{Map, ParseOptions, Value, json, parse, parse_slice, parse_with};

/// About 1 MB of hotel bookings: nested objects, escaped and non-ASCII strings, integers
/// and floats.
fn document() -> Value {
    let mut bookings = Vec::new();
    let mut size = 0;
    let mut i = 0u64;
    while size < 1_000_000 {
        let mut extras = Map::new();
        extras.insert("breakfast", i.is_multiple_of(2));
        extras.insert("late_checkout", i.is_multiple_of(3));
        let b = json!({
            "id": i,
            "code": format!("BK-{i:07}"),
            "guest": {
                "name": format!("Nguyễn Văn \"An\" #{i}"),
                "email": format!("guest{i}@example.com"),
                "vip": i.is_multiple_of(10)
            },
            "note": "Late check-in\nRoom far from the \\ lift\t(quiet)",
            "rooms": [
                {"number": 100 + i % 50, "rate": 1_250_000.5 + i as f64 * 0.25, "nights": i % 7 + 1},
                {"number": 200 + i % 30, "rate": 980_000.75, "nights": 2}
            ],
            "total": (i as f64) * 1234.5678 + 0.01,
            "discount": -0.05,
            "tags": ["direct", "mobile", "repeat"],
            "extras": extras,
            "cancelled": null
        });
        size += dl_json::to_string(&b).len() + 1;
        bookings.push(b);
        i += 1;
    }
    Value::Array(bookings)
}

fn main() {
    let mut b = Bench::from_args();
    let doc = document();
    let compact = dl_json::to_string(&doc);
    let pretty = dl_json::to_string_pretty(&doc);
    let small = r#"{"id":"INV-1","customer":"ACME","lines":[{"sku":"A","qty":2,"price":9.99}],"paid":true}"#;
    println!(
        "document: {} records, {} bytes compact, {} bytes pretty",
        doc.as_array().map_or(0, Vec::len),
        compact.len(),
        pretty.len()
    );
    let raw = ParseOptions {
        keep_raw_numbers: true,
        ..ParseOptions::default()
    };
    b.throughput("parse 1 MB compact", compact.len() as u64, || {
        parse(&compact).unwrap()
    });
    b.throughput("parse 1 MB pretty", pretty.len() as u64, || {
        parse(&pretty).unwrap()
    });
    b.throughput(
        "parse_slice 1 MB (UTF-8 check)",
        compact.len() as u64,
        || parse_slice(compact.as_bytes()).unwrap(),
    );
    b.throughput("parse_with raw numbers 1 MB", compact.len() as u64, || {
        parse_with(&compact, raw).unwrap()
    });
    b.throughput("to_string 1 MB", compact.len() as u64, || {
        dl_json::to_string(&doc)
    });
    b.throughput("to_string_pretty 1 MB", pretty.len() as u64, || {
        dl_json::to_string_pretty(&doc)
    });
    // Allocation baseline: cloning and dropping the same tree, no parsing at all.
    b.throughput(
        "clone + drop 1 MB tree (baseline)",
        compact.len() as u64,
        || doc.clone(),
    );

    // Few allocations: shows the scanner and number conversion on their own.
    let numbers: Value = (0..100_000u64)
        .map(|i| {
            if i % 2 == 0 {
                Value::from(i * 7919)
            } else {
                Value::from(i as f64 * 0.37)
            }
        })
        .collect::<Vec<_>>()
        .into();
    let numbers = dl_json::to_string(&numbers);
    b.throughput("parse numbers array", numbers.len() as u64, || {
        parse(&numbers).unwrap()
    });
    let text: Value = (0..1000)
        .map(|i| Value::from(format!("Phòng {i}: \"view biển\", tầng {}\n", i % 30).repeat(30)))
        .collect::<Vec<_>>()
        .into();
    let text = dl_json::to_string(&text);
    b.throughput("parse long strings", text.len() as u64, || {
        parse(&text).unwrap()
    });

    b.run("parse small invoice", || parse(small).unwrap());
    let small_doc = parse(small).unwrap();
    b.run("to_string small invoice", || dl_json::to_string(&small_doc));
}
