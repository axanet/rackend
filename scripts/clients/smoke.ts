// Smoke test of a typescript-fetch client generated from rackend's OpenAPI
// document (scripts/openapi-clients.sh): sign up, sign in with a password,
// create a record with a file, list, update, download the file, delete.
import { Configuration, FilesApi, NotesApi, UsersApi } from './src/index';

declare const process: { env: Record<string, string | undefined> };

const base = process.env.RACKEND_URL;
if (!base) throw new Error('set RACKEND_URL');
const email = `ts-${Date.now()}@example.com`;
const password = 'a long client password';

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(`failed: ${what}`);
  console.log(`ok: ${what}`);
}

const anon = new Configuration({ basePath: base });
await new UsersApi(anon).usersCreate({ usersCreate: { email, password } });
const auth = await new UsersApi(anon).usersAuthWithPassword({
  usersAuthWithPasswordRequest: { email, password },
});
check(typeof auth.accessToken === 'string' && auth.record.email === email, 'sign in with password');

const cfg = new Configuration({ basePath: base, accessToken: auth.accessToken });
const notes = new NotesApi(cfg);
const created = await notes.notesCreate({
  title: 'hello',
  views: 1,
  pinned: true,
  tags: ['work'],
  owner: auth.record.id,
  attachment: new Blob(['hello from typescript'], { type: 'text/plain' }),
});
check(!!created && created.title === 'hello' && created.views === 1 && created.pinned === true, 'create with a file');
const id = created!.id;
const filename = created!.attachment;
check(typeof filename === 'string' && filename.length > 0, 'file stored');

const page = await notes.notesList({ sort: '-created' });
check(page.items.length === 1 && page.items[0].id === id, 'list');

const updated = await notes.notesUpdate({ id, title: 'updated', views: 2 });
check(!!updated && updated.title === 'updated' && updated.views === 2, 'update');

const blob = await new FilesApi(cfg).filesGet({ collection: 'notes', id, filename: filename! });
check((await blob.text()) === 'hello from typescript', 'download the file');

await notes.notesDelete({ id });
check((await notes.notesList({})).items.length === 0, 'delete');
console.log('typescript client smoke test passed');
