"""Smoke test of a Python client generated from rackend's OpenAPI document
(scripts/openapi-clients.sh): sign up, sign in with a password, create a
record with a file, list, update, download the file, delete."""

import os
import time

import rackend_client as rc

base = os.environ["RACKEND_URL"]
email = f"py-{int(time.time() * 1000)}@example.com"
password = "a long client password"


def check(ok: bool, what: str) -> None:
    if not ok:
        raise SystemExit(f"failed: {what}")
    print(f"ok: {what}")


with rc.ApiClient(rc.Configuration(host=base)) as anon:
    users = rc.UsersApi(anon)
    users.users_create(rc.UsersCreate(email=email, password=password))
    auth = users.users_auth_with_password(rc.UsersAuthWithPasswordRequest(email=email, password=password))
check(bool(auth.access_token) and auth.record.email == email, "sign in with password")

with rc.ApiClient(rc.Configuration(host=base, access_token=auth.access_token)) as client:
    notes = rc.NotesApi(client)
    created = notes.notes_create(
        title="hello",
        views=1,
        pinned=True,
        tags=["work"],
        owner=auth.record.id,
        attachment=("hello.txt", b"hello from python"),
    )
    check(created is not None and created.title == "hello" and created.views == 1 and created.pinned, "create with a file")
    note_id = created.id
    filename = created.attachment
    check(bool(filename), "file stored")

    page = notes.notes_list(sort="-created")
    check(len(page.items) == 1 and page.items[0].id == note_id, "list")

    updated = notes.notes_update(id=note_id, title="updated", views=2)
    check(updated is not None and updated.title == "updated" and updated.views == 2, "update")

    data = rc.FilesApi(client).files_get(collection="notes", id=note_id, filename=filename)
    check(bytes(data) == b"hello from python", "download the file")

    notes.notes_delete(id=note_id)
    check(len(notes.notes_list().items) == 0, "delete")
print("python client smoke test passed")
