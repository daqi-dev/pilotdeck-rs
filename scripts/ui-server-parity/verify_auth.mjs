// Offline verification using the already installed TS fixture dependencies.
// Credentials arrive only on stdin; output contains booleans, never secrets.
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { createECDH } from 'node:crypto';
const require = createRequire(join(process.argv[2], 'ui/package.json'));
const bcrypt = require('bcrypt');
let input = '';
for await (const chunk of process.stdin) input += chunk;
const data = JSON.parse(input);
const passwords = await Promise.all(data.passwords.map(async ({ hash, password }) =>
  /^\$2[aby]\$12\$[./A-Za-z0-9]{53}$/.test(hash) && await bcrypt.compare(password, hash)));
const vapid = data.vapid.map(({ public_key, private_key }) => {
  try {
    const ecdh = createECDH('prime256v1');
    ecdh.setPrivateKey(Buffer.from(private_key, 'base64url'));
    return ecdh.getPublicKey().equals(Buffer.from(public_key, 'base64url'));
  } catch { return false; }
});
console.log(JSON.stringify({ passwords, vapid }));
