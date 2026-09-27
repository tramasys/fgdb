//! Explicit source verification. No line-table scans during stepping or repaint.

use super::*;
use crate::source::verification::Evidence;
use std::{sync::Arc, time::Duration};

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>) {
    let weak = Rc::downgrade(ui);
    let client = Rc::clone(client);

    ui.connect_source_verification(move |path, contents, reply| {
        request(weak.clone(), &client, path, contents, reply);
    });
}

fn request(
    ui: Weak<Ui>,
    client: &MiClient,
    path: PathBuf,
    contents: Arc<String>,
    reply: Rc<dyn Fn(String)>,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(requests) = stop_requests(
        &ui,
        client,
        current_ui.model.current_stop_refresh_generation(),
    ) else {
        reply("Unverified — pause the target before checking source".into());
        return;
    };

    let revision = current_ui.model.symbols.revision();
    let script = source_script(&path);
    let command = crate::debugger::console_command(&format!(
        "python exec({}, {{}})",
        crate::debugger::quote(&script)
    ));

    let response_requests = requests.clone();
    let response_reply = Rc::clone(&reply);

    let result = requests.frame(&command).capture(move |_, record, output| {
        let Some(evidence) = (record.is_done() && record.field("fgdb-output-truncated").is_none())
            .then(|| parse_evidence(&output))
            .flatten()
        else {
            response_reply(format!(
                "Unverified — {}",
                record.error_message().unwrap_or(
                    "No unique loaded debug object with a build ID was found for this source"
                )
            ));
            return;
        };

        let receiver =
            crate::background::submit_result(crate::background::Priority::Background, move || {
                crate::source::verification::verify(evidence, &contents)
            });

        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(error) => {
                response_reply(format!("Unverified — {error}"));
                return;
            }
        };

        gtk::glib::spawn_future_local(async move {
            let current = || {
                response_requests.is_current()
                    && ui
                        .upgrade()
                        .is_some_and(|ui| ui.model.symbols.revision() == revision)
            };

            let result =
                crate::background::receive_current(receiver, Duration::from_secs(5), &current)
                    .await;
            let status = match result {
                Ok(Ok(true)) if current() => {
                    "Verified — displayed source matches the compiled DWARF checksum".into()
                }
                Ok(Ok(false)) if current() => {
                    "Mismatched — displayed source differs from the compiled DWARF checksum".into()
                }
                Ok(Err(error)) if current() => format!("Unverified — {error}"),
                _ => "Unverified — verification expired or the debugger context changed".into(),
            };

            response_reply(status);
        });
    });

    if let Err(error) = result {
        reply(format!("Unverified — {error}"));
    }
}

fn source_script(path: &Path) -> String {
    let path = crate::debugger::quote(&path.to_string_lossy());
    let quoted = crate::debugger::quote(&path);

    format!(
        r#"import gdb
path={path}
lines=gdb.execute_mi('-symbol-list-lines', path)['lines']
line=next(int(row['line']) for row in lines if int(row['line']) > 0)
quoted={quoted}
rest, locations=gdb.decode_line(quoted+':'+str(line))
assert not rest and locations and len(locations) <= 64, 'Ambiguous source locations'
matches=set()
for location in locations:
 s=location.symtab
 if s is not None:
  o=s.objfile
  matches.add((s.filename, o.filename, o.build_id or ''))
assert len(matches) == 1, 'Multiple debug objects match this source'
filename, object_, build_id=matches.pop()
assert build_id, 'Loaded debug object has no build ID'
gdb.write('FGDB_SOURCE '+filename.encode('utf-8').hex()+' '+object_.encode('utf-8').hex()+' '+build_id+'\n')
"#
    )
}

fn parse_evidence(output: &str) -> Option<Evidence> {
    let mut lines = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_SOURCE "));
    let mut fields = lines.next()?.split_whitespace();
    let filename = fields.next()?;
    let object = fields.next()?;
    let build_id = fields.next()?;

    if fields.next().is_some()
        || lines.next().is_some()
        || filename.len() > 16384
        || object.len() > 16384
        || build_id.is_empty()
        || build_id.len() > 128
        || !build_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }

    Some(Evidence {
        filename: String::from_utf8(decode_hex(filename)?).ok()?.into(),
        object: String::from_utf8(decode_hex(object)?).ok()?.into(),
        build_id: build_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_or_unidentified_debug_objects() {
        assert!(parse_evidence("FGDB_SOURCE 61 62 aabb").is_some());
        assert!(parse_evidence("FGDB_SOURCE 61 62 unknown").is_none());
        assert!(parse_evidence("FGDB_SOURCE 61 62 aabb\nFGDB_SOURCE 61 62 aabb").is_none());
    }

    #[test]
    #[ignore = "requires Clang, readelf and Python-enabled GDB"]
    fn live_source_checksums_verify_mismatch_and_build_identity() {
        use std::process::Command;

        let directory =
            gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-source-check-XXXXXX")).unwrap();
        let source = directory.join("source with spaces.c");
        let object = directory.join("target");
        let contents = "int main(void) { return 0; }\n";
        std::fs::write(&source, contents).unwrap();

        assert!(
            Command::new("clang")
                .args(["-g", "-gdwarf-5", "-Wl,--build-id"])
                .arg(&source)
                .arg("-o")
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );

        let output = crate::bounded::process::output(
            Command::new("gdb")
                .args(["--nx", "--batch", "-ex", "set debuginfod enabled off"])
                .arg(&object)
                .arg("-ex")
                .arg(format!(
                    "python exec({}, {{}})",
                    crate::debugger::quote(&source_script(&source))
                )),
            Duration::from_secs(10),
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        let evidence = || parse_evidence(&output).unwrap();
        assert!(crate::source::verification::verify(evidence(), contents).unwrap());
        assert!(!crate::source::verification::verify(evidence(), "different source").unwrap());
        let mut wrong = evidence();
        wrong.build_id = "00".repeat(20);
        assert!(crate::source::verification::verify(wrong, contents).is_err());
        std::fs::remove_file(object).unwrap();
        std::fs::remove_file(source).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
