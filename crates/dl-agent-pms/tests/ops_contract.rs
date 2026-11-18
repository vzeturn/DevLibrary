//! `packages/shared/src/ops/fixtures/rejections.json`: ops refused before any database
//! call. The pools point at a closed port, so a refusal that touched the database would
//! come back DB_UNAVAILABLE instead of the expected code.

use std::path::PathBuf;
use std::sync::Arc;

use dl_agent_pms::ctx::Ctx;
use dl_agent_pms::db::{self, Pool};
use dl_agent_pms::exec::{Executor, Limits, SqlExecutor};
use dl_agent_pms::json;
use dl_agent_pms::log::Logger;
use dl_agent_pms::ops::Manifest;
use dl_agent_pms::wire::{self, Frame};

#[test]
fn rejections_never_touch_the_database() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/ops/rejections.json");
    let fx = json::parse(&std::fs::read(path).unwrap()).unwrap();
    let closed: dl_tds::Config = "sqlserver://sa:x@127.0.0.1:1".parse().unwrap();
    let mut dbs = db::Pools::new();
    for name in [db::DB_PMS, db::DB_FO] {
        dbs.insert(
            name.to_owned(),
            Arc::new(Pool::new(name, closed.clone(), 1)),
        );
    }
    let e = SqlExecutor {
        manifest: Arc::new(Manifest::load().unwrap()),
        dbs,
        limits: Limits::default(),
        log: Logger::discard(),
    };
    let cases = fx["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let Frame::Op(op) = wire::decode_json(json::to_string(&c["op"]).as_bytes()).unwrap() else {
            panic!("{name}: not an op");
        };
        let res = e.execute(&Ctx::background(), &op);
        let err = res
            .error
            .clone()
            .unwrap_or_else(|| panic!("{name}: no error"));
        assert_eq!(
            (res.status.as_str(), err.code.as_str(), err.message.as_str()),
            (
                c["error"]["status"].as_str().unwrap(),
                c["error"]["code"].as_str().unwrap(),
                c["error"]["message"].as_str().unwrap()
            ),
            "{name}"
        );
        assert_eq!(res.id, op.id);
        wire::encode(&Frame::Result(res)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}
