# dl-tds

A SQL Server client (TDS 7.4, and TDS 8 with strict encryption) with no dependencies:
login with SQL authentication or, on Windows, integrated authentication (SSPI), plain
batches, parameterised queries through
`sp_executesql`, every common column type, multiple result sets, `RETURN` values, server
messages and query cancellation. TLS goes through [`dl-tls`](../dl-tls) (SChannel on
Windows, system OpenSSL on Linux).

Tested against SQL Server 2022 (Linux container) on Windows and Linux, and compared value
by value with go-mssqldb v1.10.0. It is written for code ported from Go's `database/sql`
with go-mssqldb: connection strings, encryption defaults and the conversions of
[`Row::get`](Row::get) follow go-mssqldb, so the same query reads the same data.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `github.com/microsoft/go-mssqldb` (+ `golang-sql/civil`, `shopspring/decimal`, `x/crypto` md4) |
| .NET      | `Microsoft.Data.SqlClient` (SQL and Windows integrated authentication) |
| Node.js   | `tedious`, `mssql` |
| Rust      | `tiberius` |

## Example

```rust,no_run
use dl_tds::{Client, Config, Param};

let cfg: Config = "sqlserver://smile:secret@localhost?database=SMILE_FO".parse()?;
let mut c = Client::connect(&cfg)?;

// Named parameters are sent as an RPC to sp_executesql, like go-mssqldb.
let rs = c.query(
    "EXEC dbo.sp_SelectPendingFO @Limit = @Limit, @CollectFrom = @CollectFrom",
    &[Param::new("Limit", 50i32), Param::new("CollectFrom", None::<dl_time::DateTime>)],
)?;
for row in rs.rows() {
    let id: String = row.get(0)?;
    println!("{id}");
}

// A procedure that returns several result sets.
let rs = c.query("EXEC dbo.sp_FetchFO @ID = @ID", &[Param::new("ID", "12345")])?;
for (i, set) in rs.sets().iter().enumerate() {
    println!("set {i}: {} rows, {} columns", set.rows().len(), set.columns().len());
}

// Plain batch (no parameters), e.g. DDL; errors carry the line number.
match c.batch("CREATE OR ALTER PROCEDURE dbo.p AS SELECT 1") {
    Err(dl_tds::Error::Server { number, line, message, .. }) => {
        eprintln!("error {number} at line {line}: {message}")
    }
    other => { other?; }
}
let affected = c.execute("UPDATE dbo.t SET x = 1 WHERE id = @id", &[Param::new("id", 7i64)])?;
# let _ = affected;
# Ok::<(), dl_tds::Error>(())
```

## Connection strings

Both go-mssqldb forms are accepted, plus a builder (`Config::new(host).credentials(..)`):

```text
sqlserver://user:pass@host:1433/INSTANCE?database=SMILE_FO&encrypt=true&TrustServerCertificate=true&app+name=agent&connection+timeout=15
Server=.;Database=SMILE_POS;User Id=smile;Password=...;Encrypt=false;TrustServerCertificate=true
```

- Host `.`, `(local)` and `localhost` mean 127.0.0.1. `host\INSTANCE` (or the URL path)
  is resolved to a port through SQL Browser (UDP 1434). The default port is 1433.
- Keys are case-insensitive; URL values are percent-decoded.
- `connection timeout` bounds connecting and login; `Client::set_timeout` bounds each
  request. A request that times out sends ATTENTION, so the connection stays usable.

## Integrated authentication (Windows)

```rust,no_run
use dl_tds::{Client, Config};

// Any of these logs in as the current Windows account (DOMAIN\user):
let cfg: Config = "Server=.\\SQLEXPRESS;Database=master;Integrated Security=SSPI".parse()?;
let _same: Config = "Server=localhost;Trusted_Connection=yes".parse()?;
let _url: Config = "sqlserver://localhost?trusted_connection=true".parse()?;
let _no_user: Config = "sqlserver://localhost/SQLEXPRESS?database=master".parse()?;
let _built = Config::new("db01").with_integrated_auth();

let mut c = Client::connect(&cfg)?;
let rs = c.query("SELECT SUSER_SNAME()", &[])?;
let me: String = rs.rows()[0].get(0)?;
println!("logged in as {me}");
# Ok::<(), dl_tds::Error>(())
```

`Integrated Security=SSPI|true|yes` or `Trusted_Connection=yes` (ADO), and
`trusted_connection=true` or a URL **without a user name** (go-mssqldb's rule) turn it on.
The client uses SSPI's `Negotiate` package with the logged-on user's credentials, so it
is Kerberos where a domain controller can issue a ticket for the server and NTLM
otherwise (`sys.dm_exec_connections.auth_scheme` shows which). The first token goes into
LOGIN7; the server's SSPI challenges are answered until it sends LOGINACK; the Kerberos
mutual-authentication reply is checked. User name and password are not sent.

The service principal name follows Microsoft.Data.SqlClient:

| Server | SPN |
|--------|-----|
| `ServerSPN=...` (also `Server SPN`) given | that value |
| `host,port` or `host:port` | `MSSQLSvc/<fqdn>:<port>` |
| `host\INSTANCE`, no port | `MSSQLSvc/<fqdn>:INSTANCE` |
| `host` | `MSSQLSvc/<fqdn>:1433` |

`<fqdn>` is the computer's DNS name (`GetComputerNameExW`) for `.`, `(local)`,
`localhost` and loopback addresses, the canonical DNS name of any other host name
(`GetAddrInfoW` with `AI_CANONNAME`, as SqlClient's `Dns.GetHostEntry`), and an IP address
as given. When no SPN is registered for that name, Negotiate falls back to NTLM.

Outside Windows `Client::connect` returns `Error::Unsupported("integrated authentication
needs Windows (SSPI)")` before opening a socket; Kerberos and NTLM are not implemented
by hand. SSPI failures are `Error::Sspi { function, status }`; error messages never
contain tokens or passwords.

## Encryption

| `encrypt=` | Mode | What is encrypted |
|-------------|------|-------------------|
| `disable` | `Encrypt::Disabled` | nothing |
| `false`, `optional`, or no key | `Encrypt::Off` (default) | the login packet only (password), if the server supports TLS |
| `true`, `mandatory` | `Encrypt::Required` | the whole session |
| `strict` | `Encrypt::Strict` | the whole session, TLS before any TDS (TDS 8, SQL Server 2022+), certificate always verified |

As in go-mssqldb, a connection string **without** `encrypt` also implies
`TrustServerCertificate=true`. With `TrustServerCertificate=true` the server certificate
is not checked: the traffic is encrypted but a machine in the middle can read it. With
`Off`, everything after the login (queries, results) travels in clear text. Use
`Required` with a trusted certificate where the network is not trusted.

Over TLS each TDS packet is written as its own TLS record; SQL Server closes encrypted
sessions where records and packets do not line up.

## Types

| SQL Server | `Value` |
|------------|---------|
| `bit` | `Bool` |
| `tinyint`, `smallint`, `int`, `bigint` | `I8`, `I16`, `I32`, `I64` |
| `real`, `float` | `F32` (non-null real), `F64` |
| `decimal`, `numeric`, `money`, `smallmoney` | `Decimal` (money at scale 4) |
| `char`, `varchar`, `text` | `String`, decoded with the column collation's code page |
| `nchar`, `nvarchar`, `ntext`, `xml` | `String` (UTF-16, invalid surrogates become U+FFFD) |
| `binary`, `varbinary`, `image`, `timestamp`, CLR UDT | `Bytes` |
| `uniqueidentifier` | `Uuid` |
| `date`, `time`, `datetime`, `smalldatetime`, `datetime2` | `Date`, `Time`, `DateTime` |
| `datetimeoffset` | `DateTimeOffset` (stored offset kept) |
| `sql_variant` | the value of its base type |
| `(max)` types | streamed as PLP chunks, any length |

Parameters: `&str`/`String` → `nvarchar(4000)` or `nvarchar(max)`, `i32` → `int`, `i64` →
`bigint`, `bool` → `bit`, `f64` → `float`, `Decimal` → `decimal(38, s)`, `DateTime` →
`datetime2(7)`, `OffsetDateTime` → `datetimeoffset(7)`, `Uuid` → `uniqueidentifier`,
`&[u8]` → `varbinary`; `None` of any of them is a typed NULL. `ParamValue::VarChar` sends
`varchar` encoded in the code page of the connection's collation (characters the code
page cannot hold become `?`, as SQL Server itself converts them).

## Stored procedures

```rust,no_run
use dl_tds::{Client, Config, Param, Value};

let cfg: Config = "sqlserver://smile:secret@localhost?database=MM_PMS".parse()?;
let mut c = Client::connect(&cfg)?;
let r = c.call(
    "dbo.MM_Op_CreateLog",
    &[
        Param::new("OpId", "3f0c…"),
        Param::output("LogId", Option::<i64>::None),
    ],
)?;
assert_eq!(r.return_status(), Some(0));
let (name, value) = &r.output_params()[0];
assert_eq!((name.as_str(), value), ("LogId", &Value::I64(1)));
# Ok::<(), dl_tds::Error>(())
```

`Client::call` sends an RPC request by procedure name (not an `EXEC` batch), so the
procedure's `RETURN` value and its `OUTPUT` parameters come back exactly as with
go-mssqldb's `sql.Out` and `mssql.ReturnStatus`, together with every result set.

Code pages: varchar bytes are decoded exactly like go-mssqldb, with full tables for 874,
1250–1258 and the code pages of the SQL sort orders. The double-byte code pages 932, 936,
949 and 950 return `Error::UnsupportedCodePage`; read such columns as `nvarchar`.

## Conversions (`Row::get`)

`row.get::<T>(i)` follows what Go's `database/sql` `Scan` does with go-mssqldb values:

- `String` accepts every non-NULL value: integers in decimal, `bool` as `true`/`false`,
  floats like Go's `strconv.FormatFloat(v, 'g', -1, 64)`, decimal and money with every
  stored decimal (`-1234.5678`), date/time as RFC 3339 with nanoseconds.
- Integer types accept integer columns (range-checked) and text, decimal or float values
  whose text form is an integer literal.
- `f64` accepts numbers and numeric text; `bool` accepts `bit`, 0/1 and Go `ParseBool`
  text; `Decimal`, `DateTime`, `OffsetDateTime`, `Date`, `Time`, `Uuid` and `Vec<u8>`
  accept their own types and the obvious neighbours (see [`FromValue`]).
- NULL into a non-`Option` type is `Error::Null`; use `Option<T>`.

Checked against go-mssqldb scanning into `sql.NullString` for 59 values (every type above,
NULLs, five collations): identical output, with two deliberate differences:

| Column | go-mssqldb into `string` | dl-tds into `String` |
|--------|--------------------------|----------------------|
| `uniqueidentifier` | the 16 raw bytes (not text) | `6F9619FF-8B86-D011-B42D-00C04FC964FF` |
| binary that is not UTF-8 | the raw bytes | `Error::Conversion`; read it as `Vec<u8>` |

## Limits

- SQL authentication everywhere; integrated authentication on Windows only (SSPI, the
  current user; no explicit Windows user name/password, no Kerberos or NTLM on Linux or
  macOS). No Azure AD / Entra ID (FEDAUTH) logins.
- Integrated authentication sends no TLS channel bindings, so a server configured with
  `Extended Protection = Required` may refuse it (the default, `Off`, works).
- No reverse DNS for SPNs: a server given by IP address gets `MSSQLSvc/<ip>:<port>`, which
  usually means NTLM; use the host name or `ServerSPN` for Kerberos.
- No MARS: one request at a time per connection; results are read completely.
- No bulk copy, table-valued parameters or Always Encrypted columns.
- Routing (Azure redirects) is reported as an error.
