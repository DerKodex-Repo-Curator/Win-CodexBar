use super::*;

#[test]
fn oversized_remains_yields_no_reset_line_and_keeps_usage() {
    // `remains` above ~8.3e15 ms cannot be represented as a timestamp:
    // `Duration::try_seconds` must return None, so `resets_at` is None and
    // the usage percentage survives (upstream #3758/#3764).
    assert_eq!(resets_at(None, Some(i64::MAX), now()), None);
}

#[test]
fn negative_remains_yields_no_reset_line() {
    assert_eq!(resets_at(None, Some(-5), now()), None);
    assert_eq!(resets_at(None, Some(0), now()), None);
}

#[test]
fn positive_remains_keeps_the_reset_line() {
    // 90_000 > 1_000_000? No: below the heuristic, treated as seconds.
    let reset = resets_at(None, Some(60), now()).expect("small remains");
    assert_eq!(reset, now() + Duration::seconds(60));
    // Above the heuristic: treated as milliseconds.
    let reset_ms = resets_at(None, Some(2_000_000), now()).expect("ms remains");
    assert_eq!(reset_ms, now() + Duration::seconds(2_000));
}

#[test]
fn end_time_in_the_future_wins_over_remains() {
    let end = now() + Duration::seconds(120);
    assert_eq!(resets_at(Some(end), Some(60), now()), Some(end));
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 3, 12, 0, 0).unwrap()
}

#[test]
fn reset_description_buckets_by_largest_whole_unit() {
    let cases: [(Option<i64>, &str); 14] = [
        (None, "Today: 00:00-24:00"),
        (Some(-10), "Today: 00:00-24:00"),
        (Some(0), "Today: 00:00-24:00"),
        (Some(1), "Resets in 1 seconds"),
        (Some(59), "Resets in 59 seconds"),
        (Some(60), "Resets in 1 minute"),
        (Some(119), "Resets in 1 minute"),
        (Some(120), "Resets in 2 minutes"),
        (Some(3_599), "Resets in 59 minutes"),
        (Some(3_600), "Resets in 1 hour"),
        (Some(7_200), "Resets in 2 hours"),
        (Some(86_399), "Resets in 23 hours"),
        (Some(86_400), "Resets in 1 day"),
        (Some(172_800), "Resets in 2 days"),
    ];
    for (offset, expected) in cases {
        let resets = offset.map(|seconds| now() + Duration::seconds(seconds));
        assert_eq!(
            reset_description("Today", "00:00-24:00", now(), resets),
            expected,
            "offset {offset:?}"
        );
    }
}

#[test]
fn time_range_resets_read_utc_plus_8_end_times() {
    let at = |y, mo, d, h, mi| Some(Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap());
    let tomorrow = Some(now() + Duration::days(1));
    let cases = [
        (
            "2026/08/03 00:00 - 2026/08/04 00:00",
            "Today",
            at(2026, 8, 3, 16, 0),
        ),
        ("2026/08/03 00:00", "today", None),
        ("garbage - later", "Today", None),
        ("10:00-23:00(UTC+8)", "5 hours", at(2026, 8, 3, 15, 0)),
        ("10:00 - 23:30", "2h", at(2026, 8, 3, 15, 30)),
        ("08:00-19:00(UTC+8)", "5 hours", tomorrow),
        ("10:00-23:00", "Month", at(2026, 8, 3, 15, 0)),
        ("10:00", "5 hours", None),
        ("10:00-25:99", "5 hours", None),
        ("10:00-23:00", "Weekly", None),
    ];
    for (range, window, expected) in cases {
        assert_eq!(
            parse_resets_at_from_time_range(range, window, now()),
            expected,
            "{range} / {window}"
        );
    }
}

#[test]
fn time_range_labels_render_in_utc_plus_8() {
    let start = Some(Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap());
    let end = Some(Utc.with_ymd_and_hms(2026, 8, 10, 5, 30, 0).unwrap());
    assert_eq!(time_range_string(start, end), "08:00-13:30(UTC+8)");
    assert_eq!(time_range_string(start, None), "N/A");
    assert_eq!(
        weekly_time_range_string(start, end).as_deref(),
        Some("08/03 08:00 - 08/10 13:30(UTC+8)")
    );
    assert_eq!(weekly_time_range_string(None, end), None);
}

fn camel_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let mut camel = String::new();
                    let mut upper = false;
                    for ch in key.chars() {
                        if ch == '_' {
                            upper = true;
                        } else if upper {
                            camel.extend(ch.to_uppercase());
                            upper = false;
                        } else {
                            camel.push(ch);
                        }
                    }
                    (camel, camel_keys(value))
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(camel_keys).collect()),
        other => other.clone(),
    }
}

#[test]
fn remains_lanes_read_every_snake_and_camel_key() {
    let snake = serde_json::json!({
        "model_remains": [{
            "model_name": "General",
            "current_interval_total_count": 200,
            "current_interval_usage_count": 50,
            "current_interval_status": 0,
            "start_time": 1785754800,
            "end_time": 1785758340,
            "remains_time": 600,
            "current_weekly_total_count": 1000,
            "current_weekly_usage_count": 100,
            "current_weekly_remaining_percent": 40,
            "current_weekly_status": 0,
            "weekly_start_time": 1785585600,
            "weekly_end_time": 1786190400,
            "weekly_remains_time": 30
        }]
    });
    let camel = camel_keys(&snake);
    assert!(camel["modelRemains"][0].get("weeklyRemainsTime").is_some());

    let rows_for = |json: &Value| {
        let MiniMaxCodingPlanSnapshot::Remains { plan_name, rows } =
            parse_coding_plan_value(json, now()).unwrap()
        else {
            panic!("expected Remains");
        };
        assert_eq!(plan_name, None);
        rows
    };
    let rows = rows_for(&snake);
    assert_eq!(format!("{rows:?}"), format!("{:?}", rows_for(&camel)));
    assert_eq!(rows.len(), 2);

    let interval = &rows[0];
    assert_eq!(interval.model_name, "General");
    assert_eq!(interval.service_type, "general");
    assert_eq!(interval.window_type, "Custom");
    // usage_count is the remaining count: (200 - 50) / 200.
    assert_eq!(interval.percent, 75.0);
    assert_eq!(interval.window_minutes, Some(59));
    assert_eq!(interval.resets_at, Some(now() + Duration::seconds(600)));
    assert_eq!(
        interval.reset_description.as_deref(),
        Some("Resets in 10 minutes")
    );
    assert!(!interval.is_weekly);

    let weekly = &rows[1];
    assert_eq!(weekly.window_type, "Weekly");
    assert_eq!(weekly.percent, 60.0);
    assert_eq!(weekly.window_minutes, Some(10080));
    assert_eq!(weekly.resets_at, Some(now() + Duration::days(5)));
    assert_eq!(
        weekly.reset_description.as_deref(),
        Some("Resets in 5 days")
    );
    assert!(weekly.is_weekly);
}

#[test]
fn plan_name_infers_plus_from_an_unavailable_video_lane() {
    let entries = |video_remaining_percent: i64| {
        serde_json::json!({
            "model_remains": [
                {"model_name": "MiniMax-M2", "current_interval_total_count": 10,
                 "current_interval_usage_count": 5},
                {"model_name": " Video ", "current_interval_status": 3,
                 "current_interval_total_count": 0, "current_interval_usage_count": 0,
                 "current_interval_remaining_percent": video_remaining_percent}
            ]
        })
    };
    for (remaining, expected) in [(100, Some("Plus")), (50, None)] {
        let snake = entries(remaining);
        assert_eq!(
            parse_plan_name_from_data(&snake).as_deref(),
            expected,
            "snake {remaining}"
        );
        assert_eq!(
            parse_plan_name_from_data(&camel_keys(&snake)).as_deref(),
            expected,
            "camel {remaining}"
        );
    }
}

#[test]
fn parses_remains_json_snake_case() {
    let json = serde_json::json!({
        "data": {
            "current_subscribe_title": "MiniMax Pro",
            "model_remains": [{
                "model_name": "General",
                "current_interval_total_count": 100,
                "current_interval_usage_count": 73,
                "start_time": 1785792000,
                "end_time": 1785878400,
                "current_interval_status": 0,
                "current_weekly_total_count": 1000,
                "current_weekly_usage_count": 850,
                "current_weekly_remaining_percent": 85,
                "weekly_start_time": 1785792000,
                "weekly_end_time": 1786396800,
                "current_weekly_status": 0
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { plan_name, rows } = snapshot else {
        panic!("expected Remains");
    };
    assert_eq!(plan_name.as_deref(), Some("MiniMax Pro"));
    // remaining percent 73 → used 27
    assert!((rows[0].percent - 27.0).abs() < 0.01);
    // window_minutes = (end-start)/60 = 86400/60 = 1440
    assert_eq!(rows[0].window_minutes, Some(1440));
    assert!(rows[0].resets_at.is_some());
    // General is text-gen → weekly row generated
    assert_eq!(rows.len(), 2);
    assert!(rows[1].is_weekly);
}

#[test]
fn parses_counts_only_entry() {
    let json = serde_json::json!({
        "data": {
            "model_remains": [{
                "model_name": "General",
                "current_interval_total_count": 100,
                "current_interval_usage_count": 40,
                "start_time": 1722691200,
                "end_time": 1722777600,
                "current_interval_status": 0
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { rows, .. } = snapshot else {
        panic!("expected Remains");
    };
    // total=100, remaining=40 → used 60/100 = 60%
    assert!((rows[0].percent - 60.0).abs() < 0.01);
}

#[test]
fn parses_remaining_percent_with_boost_field_ignored() {
    let json = serde_json::json!({
        "data": {
            "model_remains": [{
                "model_name": "General",
                "current_interval_total_count": 0,
                "current_interval_usage_count": 0,
                "current_interval_remaining_percent": 50,
                "interval_boost_permill": 1500,
                "start_time": 1722691200,
                "end_time": 1722777600,
                "current_interval_status": 0
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { rows, .. } = snapshot else {
        panic!("expected Remains");
    };
    // remainingPercent 50 → used 50%; boost field is ignored dead plumbing
    assert!((rows[0].percent - 50.0).abs() < 0.01);
}

#[test]
fn placeholder_lane_skipped_for_video() {
    let json = serde_json::json!({
        "data": {
            "model_remains": [{
                "model_name": "video",
                "current_interval_total_count": 0,
                "current_interval_usage_count": 0,
                "current_interval_remaining_percent": 100,
                "current_interval_status": 3
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { rows, .. } = snapshot else {
        panic!("expected Remains");
    };
    // video is not text-gen → no weekly; interval is placeholder → skipped
    assert!(rows.is_empty());
}

#[test]
fn placeholder_general_weekly_kept_as_unlimited() {
    let json = serde_json::json!({
        "data": {
            "model_remains": [{
                "model_name": "General",
                "current_interval_total_count": 0,
                "current_interval_usage_count": 0,
                "current_interval_remaining_percent": 100,
                "current_interval_status": 3,
                "current_weekly_total_count": 0,
                "current_weekly_usage_count": 0,
                "current_weekly_remaining_percent": 100,
                "current_weekly_status": 3
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { rows, .. } = snapshot else {
        panic!("expected Remains");
    };
    // interval is placeholder → skipped, but weekly is unlimited → kept
    let weekly = rows.iter().find(|r| r.is_weekly).unwrap();
    assert!(weekly.is_unlimited);
    assert_eq!(weekly.reset_description.as_deref(), Some("Unlimited"));
    assert!((weekly.percent - 0.0).abs() < 0.01);
}

#[test]
fn base_resp_status_maps_to_errors() {
    // `None` expects AuthRequired; `Some` expects Other with that message.
    let cases = [
        (serde_json::json!({ "status_code": 1004 }), None),
        (
            serde_json::json!({ "status_code": 2000, "status_msg": "please log in" }),
            None,
        ),
        (
            serde_json::json!({ "status_code": 2000, "status_msg": "quota sync failed" }),
            Some("quota sync failed"),
        ),
    ];
    for (base_resp, expected) in cases {
        let json = serde_json::json!({
            "data": { "base_resp": base_resp, "model_remains": [] }
        });
        let err = parse_coding_plan_value(&json, now()).unwrap_err();
        match (expected, err) {
            (None, ProviderError::AuthRequired) => {}
            (Some(message), ProviderError::Other(actual)) => assert_eq!(actual, message),
            (expected, err) => panic!("{base_resp}: expected {expected:?}, got {err:?}"),
        }
    }
}

#[test]
fn base_resp_without_status_code_parses_fine() {
    let json = serde_json::json!({
        "data": {
            "base_resp": {},
            "model_remains": [{
                "model_name": "General",
                "current_interval_total_count": 100,
                "current_interval_usage_count": 30,
                "current_interval_remaining_percent": 70,
                "start_time": 1785792000,
                "end_time": 1785878400,
                "current_interval_status": 0
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { rows, .. } = snapshot else {
        panic!("expected Remains");
    };
    // remainingPercent 70 → used 30%; no error from absent status_code
    assert!((rows[0].percent - 30.0).abs() < 0.01);
}

#[test]
fn empty_model_remains_is_parse_error() {
    let json = serde_json::json!({
        "data": {
            "model_remains": []
        }
    });
    let err = parse_coding_plan_value(&json, now()).unwrap_err();
    assert!(matches!(err, ProviderError::Parse(_)));
}

#[test]
fn parses_multi_service_json() {
    let json = serde_json::json!({
        "data": {
            "services": [{
                "service_type": "Text Generation Pro",
                "window_type": "Today",
                "time_range": "2026/08/03 00:00 - 2026/08/04 00:00",
                "usage": 250,
                "limit": 1000
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    match snapshot {
        MiniMaxCodingPlanSnapshot::Services(rows) => {
            assert_eq!(rows.len(), 1);
            assert!((rows[0].percent - 25.0).abs() < 0.01);
            assert!(rows[0].service_type.contains("Pro"));
        }
        _ => panic!("expected Services"),
    }
}

#[test]
fn token_plan_predicate_matches_reporter_2062_message() {
    // Live reporter evidence (issue #254): the legacy remains endpoint 200s
    // with base_resp 2062 for Token Plan accounts.
    let json = serde_json::json!({
        "base_resp": {
            "status_code": 2062,
            "status_msg": "no active token plan subscription"
        }
    });
    let err = parse_coding_plan_value(&json, now()).unwrap_err();
    assert!(matches!(err, ProviderError::Other(_)));
    assert!(is_token_plan_without_coding_plan(&err));
}

#[test]
fn token_plan_predicate_rejects_unrelated_errors() {
    assert!(!is_token_plan_without_coding_plan(
        &ProviderError::AuthRequired
    ));
    assert!(!is_token_plan_without_coding_plan(&ProviderError::Other(
        "MiniMax coding plan status 2000".to_string()
    )));
    assert!(!is_token_plan_without_coding_plan(&ProviderError::Parse(
        "Missing coding plan data.".to_string()
    )));
    // Case-insensitive match on the message itself.
    assert!(is_token_plan_without_coding_plan(&ProviderError::Other(
        "No Active Token Plan Subscription".to_string()
    )));
}

#[test]
fn weekly_row_without_start_and_end_is_seven_days() {
    let json = serde_json::json!({
        "data": {
            "model_remains": [{
                "model_name": "General",
                "current_interval_total_count": 100,
                "current_interval_usage_count": 60,
                "current_interval_status": 0,
                "current_weekly_total_count": 1000,
                "current_weekly_usage_count": 900,
                "current_weekly_status": 0,
                "weekly_remains_time": 172800000
            }]
        }
    });
    let snapshot = parse_coding_plan_value(&json, now()).unwrap();
    let MiniMaxCodingPlanSnapshot::Remains { rows, .. } = snapshot else {
        panic!("expected Remains");
    };
    assert_eq!(rows.len(), 2);
    // No start/end on the interval row: no length, as before.
    assert_eq!(rows[0].window_minutes, None);
    // The weekly row has no weekly start/end but is still 7 days.
    assert!(rows[1].is_weekly);
    assert_eq!(rows[1].window_minutes, Some(10_080));
    assert!((rows[1].percent - 10.0).abs() < 0.01);
}
