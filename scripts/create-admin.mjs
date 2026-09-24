// Read a password from stdin; emit only a salted hash and administrator name.
import { randomBytes, pbkdf2Sync } from 'node:crypto'
import { TextDecoder } from 'node:util'

const userName = process.argv[2]
if (process.argv.length !== 3 || !userName || userName.length > 128 || userName.includes('\0')) {
  throw new Error('Usage: node scripts/create-admin.mjs USERNAME < password-input > credentials.json')
}
if (process.stdin.isTTY) throw new Error('Supply the password through stdin; it is never accepted as an argument.')
const chunks = []
let length = 0
for await (const chunk of process.stdin) {
  length += chunk.length
  if (length > 4096) throw new Error('Password input exceeds the LOGIN7 limit.')
  chunks.push(chunk)
}
const input = Buffer.concat(chunks)
try {
  // One terminal line ending is a delimiter, not part of the password.
  const password = new TextDecoder('utf-8', { fatal: true }).decode(input).replace(/\r?\n$/, '')
  if (password.length > 128) throw new Error('Password exceeds 128 UTF-16 code units.')
  const salt = randomBytes(32)
  const digest = pbkdf2Sync(password, salt, 600000, 32, 'sha256')
  const passwordHash = `msduck$pbkdf2-sha256$v1$${salt.toString('base64url')}$${digest.toString('base64url')}`
  process.stdout.write(JSON.stringify({ userName, passwordHash }, null, 2) + '\n')
} finally {
  input.fill(0)
  for (const chunk of chunks) chunk.fill(0)
}
