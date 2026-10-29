//! `server_core::Backend` on the pure engine: the same `(target, method, args)`
//! calls `server.py` makes, with exceptions mapped like `Handler.dispatch`.
use std::sync::Arc;

use notes_host::{Fail, Kind, Val};
use rs_engine::server_core::{Arg, Backend, Call, Failure, Target};

use crate::engine::{Engine, MAX_PDF_BYTES};
use crate::util::{dumps, from_json, key_str, meta_or, R};

pub struct RustBackend {
    pub engine: Arc<Engine>,
}

/// `Handler.dispatch` exception mapping.
pub fn failure(error: &Fail) -> Failure {
    match error.kind {
        Kind::Store => Failure::Api { code: error.code.clone(), message: error.message.clone(), status: error.status },
        Kind::Api | Kind::Conflict | Kind::Unsupported => {
            Failure::Api { code: error.code.clone(), message: error.message.clone(), status: if error.code == "conflict" { 409 } else { 503 } }
        }
        Kind::Document | Kind::PageOps => Failure::Api { code: "unsupported_document".into(), message: error.message.clone(), status: 422 },
        Kind::Disconnect => Failure::Disconnect,
        _ => Failure::Internal,
    }
}

fn arg_val(arg: Option<&Arg>) -> Val {
    match arg {
        Some(Arg::Json(v)) => from_json(v),
        Some(Arg::Str(s)) => Val::s(s.clone()),
        Some(Arg::Int(i)) => Val::Int(*i),
        Some(Arg::Bytes(b)) => Val::Bytes(b.clone()),
        Some(Arg::KwFloat(_, f)) => Val::Float(*f),
        Some(Arg::None) | None => Val::Null,
    }
}

fn arg_str(arg: Option<&Arg>) -> R<String> {
    match arg {
        Some(Arg::Str(s)) => Ok(s.clone()),
        _ => Err(Fail::type_("expected str argument")),
    }
}

fn arg_int(arg: Option<&Arg>) -> R<i64> {
    match arg {
        Some(Arg::Int(i)) => Ok(*i),
        _ => Err(Fail::type_("expected int argument")),
    }
}

impl RustBackend {
    fn store(&self) -> &rs_store::storage::StoreCore {
        &self.engine.store
    }

    /// `get_shortcuts`: schema 2, or schema 1 upgraded from its old key.
    fn shortcuts(&self) -> rs_store::db::Result<Val> {
        let store = self.store();
        let current = store.meta(rs_store::shortcuts::KEY)?;
        let legacy = if current.is_none() { store.meta(rs_store::shortcuts::LEGACY_KEY)? } else { None };
        rs_store::shortcuts::load(current, legacy)
    }

    fn invoke(&self, call: &Call) -> R<Val> {
        let e = &self.engine;
        let a = &call.args;
        let v = |i: usize| arg_val(a.get(i));
        let s = |i: usize| arg_str(a.get(i));
        match (call.target, call.method) {
            (Target::Engine, "status") => e.status(),
            (Target::Engine, "templates") => {
                let note = match a.first() {
                    Some(Arg::Str(n)) => Some(n.clone()),
                    _ => None,
                };
                e.templates(note.as_deref())
            }
            (Target::Preferences, "get_preferences") => {
                let value = meta_or(self.store(), rs_store::preferences::KEY, rs_store::preferences::default_preferences())?;
                Ok(rs_store::preferences::validate_preferences(&value)?)
            }
            (Target::Preferences, "set_preferences") => {
                let value = rs_store::preferences::validate_preferences(&v(0))?;
                self.store().set_meta(rs_store::preferences::KEY, &value)?;
                Ok(value)
            }
            (Target::Shortcuts, "get_shortcuts") => Ok(self.shortcuts()?),
            (Target::Shortcuts, "set_shortcuts") => {
                let value = rs_store::shortcuts::prepare(&v(0), || self.shortcuts())?;
                self.store().set_meta(rs_store::shortcuts::KEY, &value)?;
                Ok(value)
            }
            (Target::Store, "folders") => Ok(self.store().folders()?),
            (Target::Store, "list") => {
                let folder = match a.get(2) {
                    Some(Arg::Str(f)) => Some(Val::s(f.clone())),
                    _ => None,
                };
                Ok(Val::List(self.store().list(&s(0)?, &s(1)?, folder.as_ref())?))
            }
            (Target::Engine, "folder_change") => {
                let identity = match a.first() {
                    Some(Arg::Str(id)) => Some(id.clone()),
                    _ => None,
                };
                e.folder_change(identity.as_deref(), &v(1))
            }
            (Target::Engine, "resolve_folder") => e.resolve_folder(&s(0)?, &v(1)),
            (Target::Engine, "create_editor") => e.create_editor(&v(0), &v(1)),
            (Target::Engine, "create") => e.create(&v(0), &v(1)),
            (Target::Engine, "get") => e.get(&s(0)?),
            (Target::Engine, "save") => e.save(&s(0)?, &v(1), &v(2), &v(3)),
            (Target::Engine, "organize") => e.organize(&s(0)?, &v(1)),
            (Target::Engine, "resolve") => e.resolve(&s(0)?, &v(1)),
            (Target::Engine, "ink") => e.ink(&s(0)?),
            (Target::Engine, "apply_template") => e.apply_template(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "add_pdf") => {
                let pdf = match a.get(1) {
                    Some(Arg::Bytes(b)) => b.clone(),
                    _ => return Err(Fail::type_("expected bytes")),
                };
                e.add_pdf(&s(0)?, &pdf, &s(2)?, &Val::Int(arg_int(a.get(3))?), &Val::Null, &Val::Null)
            }
            (Target::Engine, "ink_geometry") => e.ink_geometry(&s(0)?, &s(1)?, &Val::Int(arg_int(a.get(2))?)),
            (Target::Engine, "ink_pens") => e.ink_pens(&s(0)?, &Val::Int(arg_int(a.get(1))?)),
            (Target::Engine, "save_ink_erase") => e.save_ink_erase(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "save_ink_move") => e.save_ink_move(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "shapes") => e.shapes(&s(0)?),
            (Target::Engine, "page_ops") => e.page_ops(&s(0)?),
            (Target::Engine, "save_page_op") => e.save_page_op(&s(0)?, &v(1), &v(2), &v(3)),
            (Target::Engine, "page_style") => e.page_style(&s(0)?),
            (Target::Engine, "save_page_background") => e.save_page_background(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "ink_draft") => e.ink_draft(&s(0)?),
            (Target::Engine, "append_ink_draft") => e.append_ink_draft(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "save_ink_draft") => e.save_ink_draft(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "save_ink") => e.save_ink(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "save_ink_from_draft") => e.save_ink_from_draft(&s(0)?, &v(1), &v(2), &v(3)),
            (Target::Engine, "editor") => e.editor(&s(0)?),
            (Target::Engine, "save_editor") => e.save_editor(&s(0)?, &v(1), &v(2), &v(3)),
            (Target::Engine, "save_title") => e.save_title(&s(0)?, &v(1), &v(2)),
            (Target::Engine, "export_file") => e.export_file(&s(0)?, &v(1)),
            (Target::Engine, "request_sync") => Ok(Val::Bool(e.request_sync())),
            (Target::Previews, "manifest") => e.previews.manifest(&s(0)?),
            (Target::Sessions, "start_login") => e.sessions.start_login(),
            (Target::Sessions, "disconnect") => e.sessions.disconnect(),
            (target, method) => Err(Fail::new(Kind::Attribute, format!("{:?}.{}", target, method))),
        }
    }
}

impl Backend for RustBackend {
    fn json(&self, call: Call) -> Result<Vec<u8>, Failure> {
        let value = self.invoke(&call).map_err(|e| failure(&e))?;
        dumps(&value).map_err(|e| failure(&e))
    }

    fn bytes_or_none(&self, call: Call) -> Result<Option<Vec<u8>>, Failure> {
        let run = || -> R<Option<Vec<u8>>> {
            let id = arg_str(call.args.first())?;
            let page = arg_int(call.args.get(1))?;
            let wait = call.args.iter().find_map(|a| if let Arg::KwFloat("wait", w) = a { Some(*w) } else { None }).unwrap_or(0.0);
            self.engine.previews.request(&id, page, 0, wait)
        };
        run().map_err(|e| failure(&e))
    }

    fn bytes_and_flag(&self, call: Call) -> Result<(Option<Vec<u8>>, bool), Failure> {
        let run = || -> R<(Option<Vec<u8>>, bool)> {
            let id = arg_str(call.args.first())?;
            let revision = arg_int(call.args.get(1))?;
            self.engine.thumbnails.request(&id, revision)
        };
        run().map_err(|e| failure(&e))
    }

    fn note_text(&self, call: Call) -> Result<(String, String), Failure> {
        let run = || -> R<(String, String)> {
            let note = self.engine.get(&arg_str(call.args.first())?)?;
            Ok((key_str(&note, "title")?.to_string(), key_str(&note, "body")?.to_string()))
        };
        run().map_err(|e| failure(&e))
    }

    fn max_pdf_bytes(&self) -> Result<u64, Failure> {
        Ok(MAX_PDF_BYTES)
    }

    fn is_word_char(&self, c: char) -> bool {
        crate::engine::is_word_char(c)
    }
}
