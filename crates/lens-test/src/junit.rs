use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::model::*;
use lens_core::{LensError, Result};

pub fn parse_junit_xml(xml_bytes: &[u8], project_name: &str) -> Result<TestRun> {
    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(false);
    // Best-effort: a single mismatched end tag must not discard the report.
    reader.config_mut().check_end_names = false;

    let mut buf = Vec::new();
    let mut cases = Vec::new();
    let mut summary = TestSummary::default();
    let mut properties = std::collections::BTreeMap::new();
    let mut suite_output = String::new();

    let mut current_name = String::new();
    let mut current_classname = String::new();
    let mut current_time = 0.0f64;
    let mut current_status = TestStatus::Passed;
    let mut current_message = None;
    let mut current_output = String::new();
    let mut current_body = String::new();
    // Which element's character data we are accumulating.
    let mut capture: Option<&'static str> = None;
    let mut capture_in_case = false;
    let mut in_testcase = false;
    let mut in_properties = false;
    // Enclosing <testsuite> names — cases get the innermost one.
    let mut suite_stack: Vec<String> = Vec::new();
    // Stack of open element tag names; a truncated or mismatched document
    // leaves this non-empty or sets `malformed` at EOF.
    let mut tag_stack: Vec<Vec<u8>> = Vec::new();
    let mut malformed = false;
    // The first element must be the JUnit root — a document without one is
    // not a test report and must not parse to an empty "complete" run.
    let mut saw_root = false;

    // Decode `&quot;`/`&amp;`-style entities: attr.value is the raw bytes.
    let decoder = reader.decoder();
    let decode_attr = |attr: &quick_xml::events::attributes::Attribute| -> String {
        attr.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).into_owned())
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                check_root(e.name().as_ref(), &mut saw_root)?;
                tag_stack.push(e.name().as_ref().to_vec());
                match e.name().as_ref() {
                    b"testsuite" => {
                        let mut suite_name = String::new();
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                suite_name = decode_attr(&attr);
                            }
                        }
                        suite_stack.push(suite_name);
                    }
                    b"testcase" => {
                        if in_testcase {
                            malformed = true;
                        }
                        in_testcase = true;
                        current_status = TestStatus::Passed;
                        current_message = None;
                        current_output.clear();
                        current_body.clear();
                        capture = None;
                        current_name.clear();
                        current_classname.clear();
                        current_time = 0.0;

                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"name" => current_name = decode_attr(&attr),
                                b"classname" => current_classname = decode_attr(&attr),
                                b"time" => {
                                    if let Ok(s) = std::str::from_utf8(&attr.value) {
                                        current_time = s
                                            .parse::<f64>()
                                            .ok()
                                            .filter(|v| v.is_finite() && *v >= 0.0)
                                            .unwrap_or(0.0);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    b"failure" => {
                        if in_testcase {
                            current_status = TestStatus::Failed;
                            capture = Some("failure");
                            current_body.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"message" {
                                    current_message = Some(decode_attr(&attr));
                                }
                            }
                        }
                    }
                    b"error" => {
                        if in_testcase {
                            current_status = TestStatus::Error;
                            capture = Some("error");
                            current_body.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"message" {
                                    current_message = Some(decode_attr(&attr));
                                }
                            }
                        }
                    }
                    b"skipped" if in_testcase => {
                        // Never downgrade a failure/error to skipped.
                        if current_status == TestStatus::Passed {
                            current_status = TestStatus::Skipped;
                        }
                        capture = Some("skipped");
                        current_body.clear();
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"message" {
                                current_message = Some(decode_attr(&attr));
                            }
                        }
                    }
                    // Suite- or case-level captured output.
                    b"system-out" | b"system-err" => {
                        capture = Some("system");
                        capture_in_case = in_testcase;
                        current_body.clear();
                    }
                    b"properties" => in_properties = true,
                    b"property" if in_properties => {
                        let mut k = None;
                        let mut v = None;
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"name" => k = Some(decode_attr(&attr)),
                                b"value" => v = Some(decode_attr(&attr)),
                                _ => {}
                            }
                        }
                        if let Some(k) = k {
                            properties.insert(k, v.unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                check_root(e.name().as_ref(), &mut saw_root)?;
                if e.name().as_ref() == b"testcase" {
                    let mut name = String::new();
                    let mut classname = String::new();
                    let mut time = 0.0;

                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"name" => name = decode_attr(&attr),
                            b"classname" => classname = decode_attr(&attr),
                            b"time" => {
                                if let Ok(s) = std::str::from_utf8(&attr.value) {
                                    time = s
                                        .parse::<f64>()
                                        .ok()
                                        .filter(|v| v.is_finite() && *v >= 0.0)
                                        .unwrap_or(0.0);
                                }
                            }
                            _ => {}
                        }
                    }

                    let suite = suite_stack.last().cloned().unwrap_or_default();
                    let identity = make_identity(&suite, &classname, &name);

                    summary.total += 1;
                    summary.passed += 1;
                    summary.duration_sec += time;

                    cases.push(TestCase {
                        identity,
                        name,
                        classname,
                        suite,
                        status: TestStatus::Passed,
                        duration_sec: time,
                        message: None,
                        output: None,
                    });
                } else if in_testcase {
                    match e.name().as_ref() {
                        b"failure" => {
                            current_status = TestStatus::Failed;
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"message" {
                                    current_message = Some(decode_attr(&attr));
                                }
                            }
                        }
                        b"error" => {
                            current_status = TestStatus::Error;
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"message" {
                                    current_message = Some(decode_attr(&attr));
                                }
                            }
                        }
                        b"skipped" => {
                            if current_status == TestStatus::Passed {
                                current_status = TestStatus::Skipped;
                            }
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"message" {
                                    current_message = Some(decode_attr(&attr));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if e.name().as_ref() == b"property" && in_properties {
                    let mut k = None;
                    let mut v = None;
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"name" => k = Some(decode_attr(&attr)),
                            b"value" => v = Some(decode_attr(&attr)),
                            _ => {}
                        }
                    }
                    if let Some(k) = k {
                        properties.insert(k, v.unwrap_or_default());
                    }
                }
            }
            Ok(Event::Text(ref e)) => {
                if capture.is_some() {
                    // Honor the document's declared encoding and entities.
                    let raw = decoder
                        .decode(e)
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(e).into_owned());
                    let text = quick_xml::escape::unescape(&raw)
                        .map(|c| c.into_owned())
                        .unwrap_or(raw);
                    current_body.push_str(&text);
                }
            }
            Ok(Event::GeneralRef(ref e)) => {
                if capture.is_some() {
                    if let Ok(Some(ch)) = e.resolve_char_ref() {
                        current_body.push(ch);
                    } else {
                        let raw = decoder
                            .decode(e.as_ref())
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| String::from_utf8_lossy(e.as_ref()).into_owned());
                        match raw.as_str() {
                            "lt" => current_body.push('<'),
                            "gt" => current_body.push('>'),
                            "amp" => current_body.push('&'),
                            "apos" => current_body.push('\''),
                            "quot" => current_body.push('"'),
                            other => {
                                current_body.push('&');
                                current_body.push_str(other);
                                current_body.push(';');
                            }
                        }
                    }
                }
            }
            Ok(Event::CData(ref e)) => {
                if capture.is_some() {
                    let text = decoder
                        .decode(e)
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(e).into_owned());
                    current_body.push_str(&text);
                }
            }
            Ok(Event::End(ref e)) => {
                // Flush captured character data when a tracked element closes.
                let name = e.name();
                let name = name.as_ref();
                if tag_stack.last().map(|t| t.as_slice()) == Some(name) {
                    tag_stack.pop();
                } else {
                    malformed = true;
                    if let Some(pos) = tag_stack.iter().rposition(|t| t.as_slice() == name) {
                        tag_stack.truncate(pos);
                    }
                }

                let flushed_kind = capture
                    .take_if(|k| {
                        matches!(
                            (*k, name),
                            ("failure", b"failure")
                                | ("error", b"error")
                                | ("skipped", b"skipped")
                                | ("system", b"system-out")
                                | ("system", b"system-err")
                        )
                    })
                    .or_else(|| {
                        if name == b"testcase" && in_testcase && capture.is_some() {
                            malformed = true;
                            capture.take()
                        } else {
                            None
                        }
                    });

                if let Some(kind) = flushed_kind {
                    let body = current_body.trim().to_string();
                    current_body.clear();
                    match kind {
                        "system" if capture_in_case => {
                            if !body.is_empty() {
                                if !current_output.is_empty() {
                                    current_output.push('\n');
                                }
                                current_output.push_str(&body);
                            }
                        }
                        "system" => {
                            // suite-level <system-out>/<system-err>
                            if !body.is_empty() {
                                if !suite_output.is_empty() {
                                    suite_output.push('\n');
                                }
                                suite_output.push_str(&body);
                            }
                        }
                        _ => {
                            // Use the element body as the message when the
                            // `message` attribute was absent.
                            if current_message.is_none() && !body.is_empty() {
                                current_message = Some(body);
                            }
                        }
                    }
                }
                if name == b"properties" {
                    in_properties = false;
                }
                if name == b"testsuite" {
                    if in_testcase {
                        in_testcase = false;
                        capture = None;
                        malformed = true;
                    }
                    suite_stack.pop();
                }
                if name == b"testcase" && in_testcase {
                    in_testcase = false;

                    let suite = suite_stack.last().cloned().unwrap_or_default();
                    let identity = make_identity(&suite, &current_classname, &current_name);

                    summary.total += 1;
                    match current_status {
                        TestStatus::Passed => summary.passed += 1,
                        TestStatus::Failed => summary.failed += 1,
                        TestStatus::Error => summary.errors += 1,
                        TestStatus::Skipped => summary.skipped += 1,
                    }
                    summary.duration_sec += current_time;

                    cases.push(TestCase {
                        identity,
                        name: current_name.clone(),
                        classname: current_classname.clone(),
                        suite,
                        status: current_status,
                        duration_sec: current_time,
                        message: current_message.clone(),
                        output: if current_output.is_empty() {
                            None
                        } else {
                            Some(current_output.clone())
                        },
                    });
                }
            }
            Ok(Event::Eof) => {
                if !saw_root {
                    return Err(LensError::InvalidInput {
                        message: "not a JUnit XML report: no <testsuites>/<testsuite> root element"
                            .to_string(),
                    });
                }
                break;
            }
            Err(e) => {
                return Err(LensError::InvalidInput {
                    message: format!("XML parsing error: {}", e),
                });
            }
            _ => {}
        }
        buf.clear();
    }

    // Deterministic content-addressed run id: identical inputs produce
    // identical ids so reports diff cleanly across machines and time.
    let run_id = {
        let mut h = lens_core::IncrementalHasher::new();
        h.update(project_name.as_bytes());
        for c in &cases {
            h.update(c.identity.as_bytes());
            h.update(&[c.status as u8]);
        }
        let digest = h.finish();
        format!("{}-{}", project_name, &digest[..12])
    };

    Ok(TestRun {
        schema: RUN_SCHEMA_V1.to_string(),
        producer: TestProducer {
            name: "testlens".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        run_id,
        project: project_name.to_string(),
        collected_at: lens_core::time::utc_now_iso(),
        complete: tag_stack.is_empty() && !malformed,
        summary,
        cases,
        properties,
        suite_output: if suite_output.is_empty() {
            None
        } else {
            Some(suite_output)
        },
    })
}

/// Case identity includes the enclosing suite so identically-named cases
/// in different suites do not collide.
fn make_identity(suite: &str, classname: &str, name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !suite.is_empty() {
        parts.push(suite);
    }
    if !classname.is_empty() {
        parts.push(classname);
    }
    parts.push(name);
    parts.join("::")
}

/// Reject a document whose first element is not a JUnit root.
fn check_root(name: &[u8], saw_root: &mut bool) -> Result<()> {
    if *saw_root {
        return Ok(());
    }
    *saw_root = true;
    if !matches!(name, b"testsuites" | b"testsuite") {
        return Err(LensError::InvalidInput {
            message: format!(
                "not a JUnit XML report: root element is <{}>, expected <testsuites>/<testsuite>",
                String::from_utf8_lossy(name)
            ),
        });
    }
    Ok(())
}
