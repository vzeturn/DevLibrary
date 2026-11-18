package migrate

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"fmt"
	"strings"

	"github.com/vzeturn/smile-pms/apps/site-agent/internal/bundle"
	"github.com/vzeturn/smile-pms/apps/site-agent/internal/wire"
)

// Ledger row statuses. A row is "running" only for a file that runs outside a
// transaction: it is committed before the file starts so a crash leaves a trace, and
// becomes "applied" when the file finishes. A file run in a transaction writes its
// "applied" row in that transaction and never leaves a "running" one.
const (
	StatusApplied = "applied"
	StatusRunning = "running"
)

// Ledger row sources.
const (
	SourceAgent   = "agent"
	SourceAdopted = "adopted" // seeded from MM_MigrationHistory (migrate.mjs) on first run
)

// ensureLedgerSQL creates the ledger; it is idempotent and runs before every read.
const ensureLedgerSQL = `IF OBJECT_ID(N'dbo.MM_SchemaVersion', N'U') IS NULL
BEGIN
  CREATE TABLE dbo.MM_SchemaVersion (
    Version      bigint       NOT NULL CONSTRAINT PK_MM_SchemaVersion PRIMARY KEY,
    Name         varchar(200) NOT NULL,
    Sha256       char(64)     NOT NULL,
    Chain        char(64)     NOT NULL,
    Status       varchar(16)  NOT NULL
                   CONSTRAINT CK_MM_SchemaVersion_Status CHECK (Status IN ('applied', 'running')),
    InTx         bit          NOT NULL,
    Source       varchar(16)  NOT NULL
                   CONSTRAINT CK_MM_SchemaVersion_Source CHECK (Source IN ('agent', 'adopted')),
    BundleId     varchar(100) NOT NULL,
    AgentVersion varchar(32)  NOT NULL,
    AppliedAt    datetime2(3) NOT NULL
                   CONSTRAINT DF_MM_SchemaVersion_AppliedAt DEFAULT SYSUTCDATETIME(),
    DurationMs   int          NULL
  );
END;`

const readLedgerSQL = `SELECT Version, Name, Sha256, Chain, Status FROM dbo.MM_SchemaVersion ORDER BY Version;`

// headSQL is the ledger head for hello.schema_version: the highest applied version, 0
// when the ledger does not exist yet.
const headSQL = `IF OBJECT_ID(N'dbo.MM_SchemaVersion', N'U') IS NULL
    SELECT CAST(0 AS bigint);
ELSE
    SELECT ISNULL(MAX(Version), 0) FROM dbo.MM_SchemaVersion WHERE Status = 'applied';`

const insertRowSQL = `INSERT INTO dbo.MM_SchemaVersion
  (Version, Name, Sha256, Chain, Status, InTx, Source, BundleId, AgentVersion, DurationMs)
VALUES (@Version, @Name, @Sha256, @Chain, @Status, @InTx, @Source, @BundleId, @AgentVersion, @DurationMs);`

const finishRowSQL = `UPDATE dbo.MM_SchemaVersion
SET Status = 'applied', AppliedAt = SYSUTCDATETIME(), DurationMs = @DurationMs, AgentVersion = @AgentVersion, BundleId = @BundleId
WHERE Version = @Version AND Status = 'running';`

// historySQL keeps MM_MigrationHistory (migrate.mjs, 001_migration_history.sql) in step,
// so an on-prem migrate.mjs run never re-applies a file the agent applied. The table is
// named only inside the inner IF: SQL Server defers resolving a missing table to the
// statement's execution, which never comes when the table is absent. Naming it in the
// outer IF's condition fails with error 208 on a database without it.
const historySQL = `IF OBJECT_ID(N'dbo.MM_MigrationHistory', N'U') IS NOT NULL
  IF NOT EXISTS (SELECT 1 FROM dbo.MM_MigrationHistory WHERE FileName = @Name)
    INSERT INTO dbo.MM_MigrationHistory (FileName, Checksum, DurationMs) VALUES (@Name, @Sha256, @DurationMs);`

const readHistorySQL = `IF OBJECT_ID(N'dbo.MM_MigrationHistory', N'U') IS NULL
    SELECT CAST(NULL AS varchar(200)), CAST(NULL AS char(64)) WHERE 1 = 0;
ELSE
    SELECT FileName, Checksum FROM dbo.MM_MigrationHistory;`

// Row is one ledger row.
type Row struct {
	Version int64
	Name    string
	Sha256  string
	Chain   string
	Status  string
}

// queryer is what both *sql.Conn and *sql.Tx offer.
type queryer interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
	QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error)
}

func readLedger(ctx context.Context, q queryer) ([]Row, error) {
	rows, err := q.QueryContext(ctx, readLedgerSQL)
	if err != nil {
		return nil, fmt.Errorf("read MM_SchemaVersion: %w", err)
	}
	defer func() { _ = rows.Close() }()
	var out []Row
	for rows.Next() {
		var r Row
		if err := rows.Scan(&r.Version, &r.Name, &r.Sha256, &r.Chain, &r.Status); err != nil {
			return nil, fmt.Errorf("read MM_SchemaVersion: %w", err)
		}
		r.Sha256, r.Chain, r.Name = strings.TrimSpace(r.Sha256), strings.TrimSpace(r.Chain), strings.TrimSpace(r.Name)
		out = append(out, r)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("read MM_SchemaVersion: %w", err)
	}
	return out, nil
}

func readHistory(ctx context.Context, q queryer) (map[string]string, error) {
	rows, err := q.QueryContext(ctx, readHistorySQL)
	if err != nil {
		return nil, fmt.Errorf("read MM_MigrationHistory: %w", err)
	}
	defer func() { _ = rows.Close() }()
	out := map[string]string{}
	for rows.Next() {
		var name, sum string
		if err := rows.Scan(&name, &sum); err != nil {
			return nil, fmt.Errorf("read MM_MigrationHistory: %w", err)
		}
		out[strings.TrimSpace(name)] = strings.TrimSpace(sum)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("read MM_MigrationHistory: %w", err)
	}
	return out, nil
}

// ledgerState is the checked ledger: Head is the last applied version, HeadChain its
// chain value, Running the "running" row after the head (nil when none).
type ledgerState struct {
	Head      int64
	HeadChain string
	Chains    map[int64]Row
	Running   *Row
}

// checkLedger recomputes the ledger's own chain from genesis: versions must run 1..n
// with no gap, every row's chain must follow from the previous one, and only the last
// row may be "running". Anything else is a corrupt or hand-edited ledger.
func checkLedger(rows []Row) (ledgerState, error) {
	st := ledgerState{HeadChain: bundle.GenesisChain, Chains: map[int64]Row{}}
	prev := bundle.GenesisChain
	for i, r := range rows {
		if r.Version != int64(i)+1 {
			return st, fail(CodeLedgerCorrupt, "ledger version %d follows %d (gap or duplicate)", r.Version, i)
		}
		if r.Chain != bundle.ChainStep(prev, r.Version, r.Name, r.Sha256) {
			return st, fail(CodeLedgerCorrupt, "ledger chain breaks at version %d", r.Version)
		}
		switch r.Status {
		case StatusApplied:
			if st.Running != nil {
				return st, fail(CodeLedgerCorrupt, "ledger version %d is applied after a running one", r.Version)
			}
			st.Head, st.HeadChain = r.Version, r.Chain
		case StatusRunning:
			if i != len(rows)-1 {
				return st, fail(CodeLedgerCorrupt, "ledger version %d is running but not the last row", r.Version)
			}
			rr := r
			st.Running = &rr
		default:
			return st, fail(CodeLedgerCorrupt, "ledger version %d has an unknown status", r.Version)
		}
		st.Chains[r.Version] = r
		prev = r.Chain
	}
	return st, nil
}

// compare checks the ledger against the bundle: the chain at the bundle's start must be
// the bundle's prev_chain, and every version both hold must carry the same link. A
// mismatch is a tampered ledger, a tampered bundle or a fork; nothing runs.
func compare(st ledgerState, m *bundle.Manifest) error {
	if st.Head < m.FromVersion {
		return fail(CodeLedgerBehind, "ledger head %d is before the bundle start %d: an earlier bundle must run first", st.Head, m.FromVersion)
	}
	at := bundle.GenesisChain
	if m.FromVersion > 0 {
		at = st.Chains[m.FromVersion].Chain
	}
	if at != m.PrevChain {
		return fail(CodeLedgerDiverged, "ledger chain at version %d differs from the bundle's prev_chain", m.FromVersion)
	}
	for _, e := range m.Migrations {
		r, ok := st.Chains[e.Seq]
		if !ok {
			break
		}
		if r.Chain != e.Chain || r.Name != e.Name || r.Sha256 != e.Sha256 {
			return fail(CodeLedgerDiverged, "ledger version %d differs from the bundle (%s)", e.Seq, e.Name)
		}
	}
	return nil
}

// adoptable returns the bundle prefix migrate.mjs already applied (by MM_MigrationHistory)
// when the ledger is empty. The history must be exactly a prefix of the bundle: a file
// recorded with another checksum, or applied after a gap, or unknown to the bundle, is
// divergence for an operator to resolve.
func adoptable(history map[string]string, b *bundle.Bundle) (int, error) {
	if len(history) == 0 {
		return 0, nil
	}
	if b.Manifest.FromVersion != 0 {
		return 0, fail(CodeNeedsOperator, "the site was migrated by migrate.mjs; adopting it needs a full bundle (from_version 0)")
	}
	n := 0
	for _, m := range b.Migrations {
		sum, ok := history[m.Name]
		if !ok {
			break
		}
		if sum != m.Sha256 && sum != crlfSha256(m.SQL) {
			return 0, fail(CodeLedgerDiverged, "MM_MigrationHistory checksum of %s differs from the bundle", m.Name)
		}
		n++
	}
	if n != len(history) {
		return 0, fail(CodeLedgerDiverged, "MM_MigrationHistory holds %d files but only the first %d bundle files form a prefix", len(history), n)
	}
	return n, nil
}

// crlfSha256 is the checksum migrate.mjs accepted for rows written from a CRLF checkout.
func crlfSha256(lf []byte) string {
	sum := sha256.Sum256([]byte(strings.ReplaceAll(string(lf), "\n", "\r\n")))
	return hex.EncodeToString(sum[:])
}

// LedgerVersion is the exec.SchemaVersionProvider over the ledger head.
type LedgerVersion struct{ DB *sql.DB }

// SchemaVersion is the highest applied version, wire.SchemaVersionUnknown (0) when the
// ledger is absent or empty, and 0 with the error when it cannot be read.
func (p LedgerVersion) SchemaVersion(ctx context.Context) (int64, error) {
	if p.DB == nil {
		return wire.SchemaVersionUnknown, nil
	}
	var v int64
	if err := p.DB.QueryRowContext(ctx, headSQL).Scan(&v); err != nil {
		return wire.SchemaVersionUnknown, fmt.Errorf("read MM_SchemaVersion: %w", err)
	}
	if v < 0 {
		return wire.SchemaVersionUnknown, nil
	}
	return v, nil
}
