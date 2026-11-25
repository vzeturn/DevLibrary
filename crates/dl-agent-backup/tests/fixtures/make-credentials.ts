// Generates tests/fixtures/credentials.json with the SRBackup hub's own encryptCredential,
// so the agent's decrypt is checked against an independent implementation (WebCrypto).
// Test-only keys; nothing here is a production secret.
//
//   cd ~/Projects/sr-backup/hub && bun <this file> > <crate>/tests/fixtures/credentials.json
import { encryptCredential } from '../../../../../SRBackup/hub/src/lib/credentialCrypto.ts'

const keys = ['test-agent-key-one', 'dGVzdC1hZ2VudC1rZXktdHdvLWJhc2U2NHVybA']
const plains = [
  "Sa$ecret'1;",
  '',
  'mật khẩu 🔐',
  '{"accessKeyId":"AKIDEXAMPLE","secretAccessKey":"test-secret","bucketName":"backups","region":"auto","remoteFolderPath":"site-a"}',
  'x'.repeat(300),
]
const out = []
for (const key of keys) {
  for (const plain of plains) {
    out.push({ key, plain, stored: await encryptCredential(plain, key) })
  }
}
console.log(JSON.stringify(out, null, 1))
