// When the unauthenticated GitHub API refuses the Worker (403/429: the limit is per IP and
// Worker IPs are shared), the ops console reads the same public facts from github.com's own
// endpoints, which that limit does not cover (RT-0289): the commits Atom feed and `.patch`
// files of a pull request or commit. Read-only, no token.

/** The API answered "rate limited". */
export function limited(status: number): boolean {
  return status === 403 || status === 429;
}

const unescape = (s: string) =>
  s.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&');

export interface FeedCommit {
  sha: string;
  date: string;
  subject: string;
}

/** Commits of `github.com/<repo>/commits/<ref>.atom`, newest first (the feed holds 20). */
export function parseCommitFeed(xml: string): FeedCommit[] {
  const out: FeedCommit[] = [];
  for (const m of xml.matchAll(/<entry>([\s\S]*?)<\/entry>/g)) {
    const e = m[1] ?? '';
    const sha = /Grit::Commit\/([0-9a-f]{40})/.exec(e)?.[1];
    if (!sha) continue;
    out.push({
      sha,
      date: /<updated>([^<]+)<\/updated>/.exec(e)?.[1] ?? '',
      subject: unescape((/<title>([\s\S]*?)<\/title>/.exec(e)?.[1] ?? '').trim()),
    });
  }
  return out;
}

export interface PatchSummary {
  commits: { sha: string; subject: string }[];
  files: string[];
}

/** Commits (`From <sha>` + `Subject:`) and changed files (`diff --git a/x b/x`) of a `.patch`. */
export function parsePatch(text: string): PatchSummary {
  const commits: { sha: string; subject: string }[] = [];
  const files = new Set<string>();
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? '';
    const from = /^From ([0-9a-f]{40}) /.exec(line);
    if (from?.[1]) commits.push({ sha: from[1], subject: '' });
    const subject = /^Subject: (?:\[PATCH[^\]]*\] )?(.*)$/.exec(line);
    const last = commits[commits.length - 1];
    if (subject && last && !last.subject) {
      let s = subject[1] ?? '';
      while ((lines[i + 1] ?? '').startsWith(' ')) s += (lines[++i] ?? '').replace(/^ /, ' ');
      last.subject = s.trim();
    }
    const diff = /^diff --git a\/(\S+) b\//.exec(line);
    if (diff?.[1]) files.add(diff[1]);
  }
  return { commits, files: [...files] };
}
