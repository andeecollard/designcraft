//! Browser startup, download metadata, and atomic import policies.

/// Query flags are case-sensitive decoded keys. Values are ignored: `?sample=false`
/// still enables sample startup, and repeated keys have the same presence semantics.
pub(crate) fn has_query_flag(query: &str, flag: &str) -> bool {
    let query = query.strip_prefix('?').unwrap_or(query);
    form_urlencoded::parse(query.as_bytes()).any(|(key, _)| key == flag)
}

pub(crate) fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        Some("designcraft") => designcraft_format::MIME,
        Some("idml") => "application/vnd.adobe.indesign-idml-package",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{has_query_flag, mime_for};

    #[test]
    fn startup_flags_match_complete_decoded_keys() {
        for flag in ["sample", "webgl"] {
            for query in [format!("?{flag}"), format!("?{flag}=false"), format!("?{flag}=1&{flag}=0"), format!("?other=value&{flag}")] {
                assert!(has_query_flag(&query, flag), "{query}");
            }
            for query in [
                String::new(),
                format!("?note={flag}"),
                format!("?{flag}d=1"),
                format!("?note=not_a_{flag}"),
                "?note=%73ample%77ebgl".to_string(),
                format!("?{flag}+extra=1"),
                format!("?{flag}%26extra=1"),
                format!("?{flag}%3Dextra=1"),
            ] {
                assert!(!has_query_flag(&query, flag), "{query}");
            }
        }
        assert!(has_query_flag("?webgl&sample", "sample"));
        assert!(has_query_flag("?webgl&sample", "webgl"));
        assert!(has_query_flag("?%73ample", "sample"));
        assert!(has_query_flag("?%77ebgl", "webgl"));
        assert!(!has_query_flag("?Sample&WEBGL", "sample"));
        assert!(!has_query_flag("?Sample&WEBGL", "webgl"));
    }

    #[test]
    fn download_mime_matches_native_package_and_existing_exports() {
        for (name, expected) in [
            ("Quarterly.designcraft", designcraft_format::MIME),
            ("日本語.multi.DEsignCraft", designcraft_format::MIME),
            ("page.pdf", "application/pdf"),
            ("page.png", "image/png"),
            ("page.jpg", "image/jpeg"),
            ("page.JPEG", "image/jpeg"),
            ("document.idml", "application/vnd.adobe.indesign-idml-package"),
            ("unknown.bin", "application/octet-stream"),
        ] {
            assert_eq!(mime_for(name), expected, "{name}");
        }
    }
}

/// Read a drop in order, publishing nothing unless every file can be read.
pub(crate) async fn read_batch<T, R, E, F, Fut>(files: impl IntoIterator<Item = T>, mut read: F) -> Result<Vec<R>, E>
where
    F: FnMut(T) -> Fut,
    Fut: std::future::Future<Output = Result<R, E>>,
{
    let mut result = Vec::new();
    for file in files {
        result.push(read(file).await?);
    }
    Ok(result)
}

#[cfg(test)]
mod batch_tests {
    #[test]
    fn failed_read_aborts_the_whole_batch() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let mut seen = Vec::new();
        let result = {
            let future = super::read_batch([1, 2, 3], |n| {
                seen.push(n);
                std::future::ready(if n == 2 { Err("read failed") } else { Ok(n) })
            });
            let mut future = std::pin::pin!(future);
            future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
        };
        assert_eq!(result, Poll::Ready(Err("read failed")));
        assert_eq!(seen, [1, 2]);
        let mut success = std::pin::pin!(super::read_batch([3, 1], |n| std::future::ready(Ok::<_, ()>(n))));
        assert_eq!(success.as_mut().poll(&mut Context::from_waker(Waker::noop())), Poll::Ready(Ok(vec![3, 1])));
    }
}

/// Process successful deliveries before read-failure alerts can block their import.
pub(crate) fn logic_with_read_failures(app: &mut designcraft_ui_egui::DesignApp, ctx: &egui::Context, unread: Vec<(Vec<String>, String)>) {
    app.logic(ctx);
    for (names, error) in unread {
        app.import_read_failed(&names, &error);
    }
}

#[cfg(test)]
mod delivery_tests {
    use designcraft_engine::Session;
    use designcraft_ui_egui::{DesignApp, ImportedFile, Inbox, Services};
    use serde_json::json;

    #[test]
    fn a_read_failure_does_not_cancel_an_independent_successful_import() {
        for count in [1, 2] {
            let inbox = Inbox::default();
            let mut app = DesignApp::new(Session::new(), Services { inbox: Some(inbox.clone()), ..Default::default() });
            app.run("file.new", json!({})).unwrap();
            let ctx = egui::Context::default();
            // Complete the font/layout startup before delivering files, as in an active browser.
            for _ in 0..2 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
                output.textures_delta.clear();
            }
            let request = app.import_request("drop");
            for index in 0..count {
                inbox.lock().unwrap().push(ImportedFile {
                    request: request.clone(),
                    name: format!("successful-{index}.svg"),
                    bytes: br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##
                        .to_vec(),
                });
            }
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                super::logic_with_read_failures(&mut app, ui.ctx(), vec![(vec!["unreadable.png".into()], "read failed".into())]);
            });
            output.textures_delta.clear();
            assert_eq!(app.session.doc().unwrap().doc.assets.len(), 1);
            if count == 2 {
                assert!(app.session.loaded.is_some());
                assert_eq!(app.session.loaded_queue.len(), 1);
            }
            let dialog = app.ui.dialog.as_ref().unwrap();
            assert_eq!(dialog.id, "alert");
            assert_eq!(dialog.fields["file"], "unreadable.png");
            assert_eq!(dialog.fields["message"], "read failed");
        }
    }
}
