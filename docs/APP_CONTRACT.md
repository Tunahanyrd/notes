# Linux Notes application contract v2

Local engine: Python module `samsung_notes`, existing Rust protocol parser and
Samsung wire helpers. Frontend: `desktop/`, React/TypeScript + Tauri. HTTP serves
the built frontend on the same loopback origin. No remote fonts, analytics or
Samsung account tokens in the UI.

Engine launch: `python3 -m samsung_notes --port 0 --runtime-file PATH --no-browser`.
PATH is a new private runtime JSON file, containing `{port,url,token,pid}`.
URL is `http://127.0.0.1:PORT/#token=LOCAL_RANDOM_SECRET`. Frontend captures this
fragment into sessionStorage, removes it with history.replaceState, and sends
`Authorization: Bearer LOCAL_RANDOM_SECRET` for every `/api/` request. This is
a local app secret, never a Samsung token. Bind only 127.0.0.1. No wildcard CORS.
Backend serves `desktop/dist`. Dev via Vite proxy `/api` to an explicit local
engine URL is optional; actual integration uses built same-origin frontend.

JSON errors: `{error:{code:string,message:string}}`, with suitable HTTP status.
Dates are epoch milliseconds or null. Note identifiers are local UUIDs; folder
identifiers are bounded opaque strings and must be URL-encoded in path segments.

Endpoints:

- `GET /api/preferences` → `{version:1,galleryMode:'grid'|'list',sortAscending:boolean,
  toolsOpen:boolean,defaultZoom:75|100|125|150}`. `PUT` the same exact object to
  persist it in the library. Display preferences are device-local, independent
  of the randomized loopback port. Browser localStorage alone is not persistence
  across native launches. Serialize writes and do not overwrite saved preferences
  from startup defaults before hydration completes.
- `GET /api/status` → `{connected,accountLabel,loginPending,loginError,sync,capabilities}`.
  Login errors are separate from synchronization errors; the UI must not label
  an unavailable Cloud endpoint as a failed Samsung login.
  `sync={state:'idle'|'syncing'|'offline'|'error',lastSyncedAt,pending,conflicts,
  progress:{processed,total},error:string|null}`. Capabilities are
  `{textEditing:true,richTextEditing:true,cloudDelete:false,attachments:false,cloudTrash:boolean,
  cloudFavorites:boolean,cloudFolders:boolean}`. Cloud organization becomes
  available after the folder graph has been imported.
- `GET /api/notes?q=&filter=all|favorites|conflicts|trash&folder=ID` → `{notes:Note[]}`.
- `GET /api/notes/:id` → `{note:Note}`; may load cloud content on demand.
- `POST /api/notes` body `{title,body}` → 201 `{note:Note}`. Local durable save,
  queued for cloud publication; UI must distinguish local save from cloud sync.
- `PUT /api/notes/:id` body `{title,body,expectedRevision:number}` → `{note}`.
  409 if local revision changed, 422 if content is not supported for editing.
- `PATCH /api/notes/:id` body exactly one of `{favorite:boolean}`, `{trashed:boolean}`,
  `{folderId:string|null}`, optionally `expectedRevision`, → `{note}`. These are
  durable Cloud intents, not local-only preferences. `pending` becomes `synced`
  only after Cloud readback; ambiguous outcomes are not resent automatically.
  Note content and organization writes are serialized. Trash is recoverable;
  permanent deletion is unsupported.
- `PATCH /api/notes/:id/title` body `{title,expectedRevision}` → `{note}`. This
  changes only the title of a supported loaded Cloud document. `titleEditable`
  is independent of body `editable`; sending a body is rejected. The frozen
  source snapshot, all pages, ink and attachments must survive readback.
- `GET /api/notes/:id/editor` → `{schemaVersion:1,revision,capabilities,model,reason}`.
  Capabilities are per-document `{titleEdit,textEdit,richStyle,inkEdit:false}`.
  Projections are cached by local revision; pending local models take precedence
  over older Cloud snapshots. Model positions are UTF-16 offsets; see EDITOR_FORMAT.
- `POST /api/notes/editor` body exactly `{title,model}` → 201 `{note}`.
- `PUT /api/notes/:id/editor` body exactly `{title,model,expectedRevision}` → `{note}`.
  Offline successive edits retain the latest model; a sent job keeps its own
  immutable model and snapshot. An older acknowledgement cannot discard a newer
  draft. Plain-text PUT cannot flatten a structured local draft.
- `GET /api/folders` → `{folders:Folder[],trash:Folder[],initialized:boolean}`.
- `POST /api/folders` body `{name,parentId?:string|null}` → 201 `{folder}`.
- `PATCH /api/folders/:id` one of `{name}`, `{parentId}`, `{trashed}` → `{folder}`.
  Only empty folders may be trashed; cycles and concurrent intents are rejected.
- `POST /api/folders/:id/resolve` body `{strategy:'use_remote'}` → `{folder|null}`.
  Reads fresh Cloud state before abandoning a failed or uncertain local intent.
- `POST /api/notes/:id/resolve` body `{strategy:'keep_local_copy'|'use_remote'}`
  → `{note}`. Preserve local text as a new note or accept remote content.
  Organization conflicts use only `use_remote`, with a fresh graph/document read.
- `GET /api/notes/:id/export` → UTF-8 plain text attachment, via authorized fetch.
- `GET /api/notes/:id/pages` → `{providerAvailable:boolean,pages:Page[]}`.
  `Page={index:number,width:number|null,height:number|null,available:boolean,
  darkMode:boolean,previewKind?:string,complete?:boolean,previewNotice?:string|null}`.
  `pdf-background` may be incomplete; show its notice when present.
  `pdf-with-notes` combines a verified PDF background and matching transparent
  saved ink layer. Indices are zero-based and preserve the source inventory,
  including unavailable pages. No remote download URLs or hashes reach the UI.
- `GET /api/notes/:id/thumbnail?revision=N` → `200 image/png` or
  `202 {state:'pending'}` with `Retry-After: 2`. Required revision is a positive
  integer; stale revisions return 409. Other 404/422/429/503 errors follow the
  page-preview contract. `X-Notes-Preview-Complete: true|false` discloses missing
  layers. Only source page zero is used; never substitute a later page. The
  bounded 320×384 box-fit thumbnail preserves alpha against the actual page
  background and is cached by account/source recipe. Cards fetch only while
  visible, with three concurrent requests and bounded pending polling.
- `GET /api/notes/:id/preview?page=0` → authenticated `200 image/png`, or
  `202 {state:'pending'}` while one bounded worker prepares the page. The UI polls
  every two seconds for at most 120 seconds; leaving the note aborts its polling.
  `404` means no source image, `422` a failed decode/download, `429` a full queue,
  and `503` an uncached page while disconnected. An error stops polling; a later
  user request may retry after five seconds. No worker retries network requests.
  PNG alpha is preserved: use black for darkMode and white otherwise, no inversion.
  The cache is private and keyed by account and source content hash. Limits come
  from `samsung_notes/preview_contract.py`: 8 MiB input, 4 Mi pixels, 64 MiB output,
  45-second decode deadline for native canvas caches. PDF input is bounded at
  128 MiB, shares the pixel/output limits and renders one page in a network-isolated
  Poppler subprocess with a 40-second deadline. Both are read-only page rendering,
  not an editable ink canvas.
- `POST /api/sync` body `{}` → 202 `{started:boolean}`. Poll status while running.
- `POST /api/auth/login` body `{}` → 202 `{started:true}`; engine opens system
  browser. No credentials entered in frontend.
- `POST /api/auth/disconnect` body `{}` → `{connected:false}`; stops this app's
  account use, retaining offline local notes. No remote revocation/deletion.

`Note={id,title,preview,body?:string,modifiedAt,createdAt,revision:number,
syncState:'synced'|'pending'|'conflict'|'error'|'local'|'uncertain',
editable:boolean,titleEditable:boolean,readOnlyReason:string|null,favorite:boolean,trashed:boolean,
folderId:string|null,metadataSync:{state:string,error:string|null},
contentLoaded:boolean,source:'cloud'|'local',error:string|null}`.
List rows may omit body. Detail includes body (logical text preview for unsupported
rich documents) and optionally `remoteBody`, `remoteTitle` for a conflict.

`Folder={id,name,parentId,color,noteCount,state:'normal'|'trashed',syncState,error}`.
Remote IDs and document payloads do not enter diagnostics or UI copy. Old local
favorites/trash from schema v1 are retained for recovery, never automatically
published as new Cloud actions. Current database schema is v5; upgrades are
transactional and retain immutable outbox snapshots.

Empty/loading/error/read-only/conflict/offline states must be real. No mock data
fallback in production. Save explicit or debounce with serialized saves and
revision updates; unsaved text must survive note switches and sync polling.

`GET/HEAD /local-fonts/:name` optionally serves five fixed Segoe UI font filenames
from a private, hash-checked local manifest. It needs no bearer token because font
bytes are not account data; strict Host/Origin checks still apply. Missing fonts
return 404 and CSS falls back to local sans-serif. There is no arbitrary-file route
or remote font request.

Ownership: root owns `samsung_notes/`, backend tests and this contract. API worker
owns `samsung_notes/cloud_api.py`, `samsung_notes/document.py` and associated new
tests. Frontend worker owns `desktop/` entirely, including standalone Cargo
workspace. Changes to this contract must be coordinated with root first.
