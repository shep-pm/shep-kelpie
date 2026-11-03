//! `[app.dogs.kelpie.triage]`, which says whether kelpie triages issues other
//! people open

use std::num::NonZeroU32;

use super::{EXAMPLE, parse, parse_err};
use crate::ports::Probability;
use crate::settings::{AuthorRole, TriagePick};

const TABLE: &str = "[app.dogs.kelpie.triage]\n";

const SPAM: &str = "[app.dogs.kelpie.triage.spam]\n";

// The example with `lines` under a triage table.
fn with(lines: &str) -> String {
    format!("{EXAMPLE}\n{TABLE}{lines}")
}

// The example with `lines` under a triage spam table.
fn with_spam(lines: &str) -> String {
    with(&format!("pick = \"all\"\n{SPAM}{lines}"))
}

#[test]
fn an_absent_triage_table_means_pick_off_wait_7_questions_10_and_no_agent() {
    let t = parse(EXAMPLE).unwrap().triage;
    assert_eq!(t.pick, TriagePick::Off);
    assert_eq!(t.wait_days, NonZeroU32::new(7).unwrap());
    assert_eq!(t.max_questions.get(), 10);
    assert_eq!(t.agent, None);
    assert_eq!(parse(&with("")).unwrap().triage, t, "an empty table");
    let partial = parse(&with("pick = \"all\"\n")).unwrap().triage;
    assert_eq!(
        partial.wait_days, t.wait_days,
        "a key left out keeps its default"
    );
    assert_eq!(partial.max_questions, t.max_questions);
    assert_eq!(partial.agent, t.agent);
}

#[test]
fn pick_takes_off_maintainer_or_all_and_refuses_any_other_word() {
    for (word, pick) in [
        ("off", TriagePick::Off),
        ("maintainer", TriagePick::Maintainer),
        ("all", TriagePick::All),
    ] {
        let settings = parse(&with(&format!("pick = \"{word}\"\n"))).unwrap();
        assert_eq!(settings.triage.pick, pick, "{word}");
    }
    let err = parse_err(&with("pick = \"some\"\n"));
    assert!(err.contains("pick") && err.contains("some"), "{err}");
}

#[test]
fn max_questions_of_zero_or_above_10_is_refused_naming_it() {
    for n in [0, 11, 50] {
        let err = parse_err(&with(&format!("max_questions = {n}\n")));
        assert!(err.contains("max_questions"), "{n}: {err}");
    }
    for n in [1, 10] {
        let t = parse(&with(&format!("max_questions = {n}\n")))
            .unwrap()
            .triage;
        assert_eq!(t.max_questions.get(), n);
    }
}

#[test]
fn zero_wait_days_is_refused() {
    let err = parse_err(&with("wait_days = 0\n"));
    assert!(err.contains("wait_days"), "{err}");
    let t = parse(&with("wait_days = 3\n")).unwrap().triage;
    assert_eq!(t.wait_days, NonZeroU32::new(3).unwrap());
}

#[test]
fn the_limits_default_to_10_a_day_2_per_author_30_days_and_20_kb() {
    let t = parse(EXAMPLE).unwrap().triage;
    assert_eq!(t.per_day, 10);
    assert_eq!(t.per_author_per_day, 2);
    assert_eq!(t.min_account_days, 30);
    assert_eq!(t.max_body_kb, 20);
    assert_eq!(parse(&with("")).unwrap().triage, t, "an empty table");
    let partial = parse(&with("pick = \"all\"\n")).unwrap().triage;
    assert_eq!(
        partial.per_day, t.per_day,
        "a key left out keeps its default"
    );
    assert_eq!(partial.per_author_per_day, t.per_author_per_day);
    assert_eq!(partial.min_account_days, t.min_account_days);
    assert_eq!(partial.max_body_kb, t.max_body_kb);
    for (key, want) in [
        ("per_day", 0u32),
        ("per_author_per_day", 1),
        ("min_account_days", 7),
        ("max_body_kb", 3),
    ] {
        let t = parse(&with(&format!("{key} = {want}\n"))).unwrap().triage;
        let got = match key {
            "per_day" => t.per_day,
            "per_author_per_day" => t.per_author_per_day,
            "min_account_days" => t.min_account_days,
            _ => t.max_body_kb,
        };
        assert_eq!(got, want, "{key} = {want}");
    }
}

#[test]
fn a_limit_that_is_negative_or_not_a_number_is_refused_naming_it() {
    for key in [
        "per_day",
        "per_author_per_day",
        "min_account_days",
        "max_body_kb",
    ] {
        for value in ["-1", "\"ten\"", "1.5", "true"] {
            let err = parse_err(&with(&format!("{key} = {value}\n")));
            assert!(err.contains(key), "{key} = {value}: {err}");
        }
    }
}

#[test]
fn a_daily_cap_past_the_records_a_count_can_see_is_refused_naming_it() {
    assert_eq!(
        u32::try_from(crate::state::TRIAGED_KEPT).unwrap(),
        crate::settings::triage::MOST_PER_DAY,
        "the caps are bounded by what the state keeps"
    );
    for key in ["per_day", "per_author_per_day"] {
        for n in [201, 1_000, u32::MAX] {
            let err = parse_err(&with(&format!("{key} = {n}\n")));
            assert!(
                err.contains(key) && err.contains("200"),
                "{key} = {n}: {err}"
            );
        }
        let t = parse(&with(&format!("{key} = 200\n"))).unwrap().triage;
        let got = if key == "per_day" {
            t.per_day
        } else {
            t.per_author_per_day
        };
        assert_eq!(got, 200, "{key} = 200 is the ceiling");
    }
}

#[test]
fn an_unknown_key_in_triage_is_refused_naming_it() {
    let err = parse_err(&with("pik = \"all\"\n"));
    assert!(err.contains("pik"), "{err}");
}

#[test]
fn an_absent_spam_table_means_no_check_and_a_threshold_of_0_8() {
    let t = parse(EXAMPLE).unwrap().triage.spam;
    assert!(t.gateway.is_none(), "no gateway, so no check");
    assert!(t.path.is_none());
    assert!(t.is_off());
    assert_eq!(t.threshold, Probability::millionths(800_000));
    assert_eq!(parse(&with("")).unwrap().triage.spam, t, "an empty table");
    assert_eq!(
        parse(&with("pick = \"all\"\n")).unwrap().triage.spam,
        t,
        "triage on and no spam table"
    );
    // A table that names no gateway is a check that is off, whatever else it sets
    let off = parse(&with_spam("threshold = 0.2\n")).unwrap().triage.spam;
    assert!(off.is_off());
    assert_eq!(off.threshold, Probability::millionths(200_000));
}

#[test]
fn a_spam_table_names_its_gateway_its_route_and_its_threshold() {
    let t = parse(&with_spam(
        "gateway = \"paddock\"\npath = \"/classify\"\nthreshold = 0.95\n",
    ))
    .unwrap()
    .triage
    .spam;
    assert_eq!(t.gateway.as_ref().unwrap().as_str(), "paddock");
    assert_eq!(t.path.as_ref().unwrap().as_str(), "/classify");
    assert_eq!(t.threshold, Probability::millionths(950_000));
    assert!(
        t.is_spam(Probability::millionths(950_000)),
        "at the threshold"
    );
    assert!(
        !t.is_spam(Probability::millionths(949_000)),
        "and just under it"
    );
    // A threshold left out keeps the default
    let t = parse(&with_spam("gateway = \"paddock\"\npath = \"/classify\"\n"))
        .unwrap()
        .triage
        .spam;
    assert_eq!(t.threshold, Probability::millionths(800_000));
}

#[test]
fn a_spam_answer_just_under_the_threshold_is_not_spam() {
    let t = parse(&with_spam("gateway = \"paddock\"\npath = \"/classify\"\n"))
        .unwrap()
        .triage
        .spam;
    // Read to the thousandth, 0.7996 stands at 0.8, which is the default
    // threshold. It is held down to the millionth at or below it, so the issue
    // that scored it is not spam.
    let just_under = Probability::new(0.7996).unwrap();
    assert_eq!(just_under, Probability::millionths(799_600));
    assert!(
        !t.is_spam(just_under),
        "{just_under} is under the threshold"
    );
    assert!(
        t.is_spam(Probability::new(0.8).unwrap()),
        "and 0.8 is at it"
    );
}

#[test]
fn a_spam_threshold_outside_0_to_1_is_refused_naming_it() {
    for value in ["-0.1", "1.5", "10", "\"high\"", "true"] {
        let err = parse_err(&with_spam(&format!("threshold = {value}\n")));
        assert!(err.contains("threshold"), "{value}: {err}");
    }
    for value in ["0", "0.0", "1", "1.0", "0.8"] {
        let t = parse(&with_spam(&format!("threshold = {value}\n")))
            .unwrap()
            .triage
            .spam;
        assert_eq!(t.threshold.value(), value.parse::<f64>().unwrap());
    }
}

#[test]
fn a_spam_gateway_with_no_route_is_refused_naming_the_route() {
    let err = parse_err(&with_spam("gateway = \"paddock\"\n"));
    assert!(
        err.contains("triage.spam.path") && err.contains("/classify"),
        "{err}"
    );
    // With no gateway there is no check, so no route is owed
    parse(&with_spam("path = \"/classify\"\n")).unwrap();
}

#[test]
fn a_spam_route_that_is_not_one_path_is_refused_naming_it() {
    for value in [
        "\"classify\"",
        "\"\"",
        "\"http://gpu:8700/classify\"",
        "\"/classify x\"",
        "\"/classify\\r\\nX-Evil: 1\"",
        "\"/classify#x\"",
    ] {
        let err = parse_err(&with_spam(&format!(
            "gateway = \"paddock\"\npath = {value}\n"
        )));
        assert!(err.contains("path"), "{value}: {err}");
    }
    for value in ["/classify", "/v1/classify", "/classify?model=spam"] {
        let t = parse(&with_spam(&format!(
            "gateway = \"paddock\"\npath = \"{value}\"\n"
        )))
        .unwrap()
        .triage
        .spam;
        assert_eq!(t.path.unwrap().as_str(), value, "{value}");
    }
}

#[test]
fn an_unknown_key_in_the_spam_table_is_refused_naming_it() {
    let err = parse_err(&with_spam("gateways = \"paddock\"\n"));
    assert!(err.contains("gateways"), "{err}");
}

// The example with its tickets taken from the local store
fn on_local_store(text: &str) -> String {
    let github = "[app.dogs.kelpie.tickets]\nsource = \"github\"";
    assert!(
        text.contains(github),
        "the example's `tickets.source` moved"
    );
    text.replace(github, "[app.dogs.kelpie.tickets]\nsource = \"local\"")
}

#[test]
fn triage_is_refused_on_the_local_store_and_allowed_with_it_off() {
    parse(&on_local_store(EXAMPLE)).unwrap();
    parse(&with("pick = \"all\"\n")).unwrap();
    for pick in ["maintainer", "all"] {
        let err = parse_err(&on_local_store(&with(&format!("pick = \"{pick}\"\n"))));
        assert!(
            err.contains("triage.pick") && err.contains("local"),
            "{err}"
        );
    }
}

#[test]
fn an_absent_authors_table_overrides_no_login() {
    let t = parse(EXAMPLE).unwrap().triage;
    assert!(t.authors.is_empty());
    assert_eq!(t.role_of("anyone"), None);
}

#[test]
fn authors_gives_a_listed_login_a_role_whatever_its_case() {
    let t = parse(&with(
        "authors = { \"Qc-Tester\" = \"reporter\", \"dev\" = \"developer\", \"boss\" = \"maintainer\" }\n",
    ))
    .unwrap()
    .triage;
    assert_eq!(t.role_of("qc-tester"), Some(AuthorRole::Reporter));
    assert_eq!(t.role_of("QC-TESTER"), Some(AuthorRole::Reporter));
    assert_eq!(t.role_of("dev"), Some(AuthorRole::Developer));
    assert_eq!(t.role_of("boss"), Some(AuthorRole::Maintainer));
    assert_eq!(t.role_of("someone-else"), None);
}

#[test]
fn an_unknown_author_role_is_refused_naming_the_setting() {
    let err = parse_err(&with("authors = { \"qc\" = \"tester\" }\n"));
    assert!(
        err.contains("triage.authors") && err.contains("tester"),
        "{err}"
    );
    let err = parse_err(&with("authors = { \"qc\" = 3 }\n"));
    assert!(err.contains("triage.authors"), "{err}");
}

#[test]
fn a_role_asks_up_to_its_own_cap_and_never_past_max_questions() {
    let t = parse(&with("max_questions = 10\n")).unwrap().triage;
    assert_eq!(AuthorRole::Maintainer.most_questions(t.max_questions), 10);
    assert_eq!(AuthorRole::Developer.most_questions(t.max_questions), 3);
    assert_eq!(AuthorRole::Reporter.most_questions(t.max_questions), 1);
    let t = parse(&with("max_questions = 2\n")).unwrap().triage;
    assert_eq!(AuthorRole::Maintainer.most_questions(t.max_questions), 2);
    assert_eq!(AuthorRole::Developer.most_questions(t.max_questions), 2);
    assert_eq!(AuthorRole::Reporter.most_questions(t.max_questions), 1);
}

#[test]
fn the_forge_association_decides_a_role_when_no_login_is_listed() {
    use crate::ports::AuthorAssociation as A;
    for (association, role) in [
        (Some(A::Owner), AuthorRole::Maintainer),
        (Some(A::Member), AuthorRole::Developer),
        (Some(A::Collaborator), AuthorRole::Developer),
        (Some(A::Contributor), AuthorRole::Developer),
        (Some(A::FirstTimeContributor), AuthorRole::Reporter),
        (Some(A::FirstTimer), AuthorRole::Reporter),
        (Some(A::None), AuthorRole::Reporter),
        (None, AuthorRole::Reporter),
    ] {
        assert_eq!(AuthorRole::of(association), role, "{association:?}");
    }
}

#[test]
fn hand_ready_is_off_when_absent_and_on_under_maintainer_or_all() {
    assert!(!parse(EXAMPLE).unwrap().triage.hand_ready);
    assert!(!parse(&with("pick = \"all\"\n")).unwrap().triage.hand_ready);
    for pick in ["maintainer", "all"] {
        let t = parse(&with(&format!("pick = \"{pick}\"\nhand_ready = true\n")))
            .unwrap()
            .triage;
        assert!(t.hand_ready, "{pick}");
    }
}

#[test]
fn hand_ready_without_a_pick_is_refused_naming_it() {
    for lines in ["hand_ready = true\n", "pick = \"off\"\nhand_ready = true\n"] {
        let err = parse_err(&with(lines));
        assert!(
            err.contains("triage.hand_ready") && err.contains("triage.pick"),
            "{lines}: {err}"
        );
    }
    let t = parse(&with("pick = \"off\"\nhand_ready = false\n"))
        .unwrap()
        .triage;
    assert!(!t.hand_ready, "false needs no pick");
}

#[test]
fn hand_ready_that_is_not_a_bool_is_refused_naming_it() {
    let err = parse_err(&with("pick = \"all\"\nhand_ready = \"yes\"\n"));
    assert!(err.contains("hand_ready"), "{err}");
}
