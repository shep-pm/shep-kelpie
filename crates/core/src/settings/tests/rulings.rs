//! `rulings.pm_decides`, which maps kinds of ruling to who decides them, and
//! the old `rulings.pm_alone` list it replaces

use super::{EXAMPLE, parse, parse_err};
use crate::settings::RulingName;

fn rulings(lines: &str) -> crate::settings::Rulings {
    parse(&format!("{EXAMPLE}\n[app.dogs.kelpie.rulings]\n{lines}\n"))
        .unwrap()
        .rulings
}

#[test]
fn a_removed_kind_in_the_list_makes_it_refuse_by_name() {
    let spilled = rulings("pm_alone = [\"local-model-spilled\"]");
    assert!(
        spilled
            .refusal()
            .is_some_and(|r| r.to_string().contains("local-model-spilled"))
    );
}

#[test]
fn only_a_merge_kind_makes_the_list_refuse() {
    let merge = rulings("pm_alone = [\"merge\"]");
    let refused = merge.refusal().expect("merge is refused").to_string();
    assert!(refused.contains("merge"), "{refused}");
    assert!(refused.contains("rulings.pm_alone"), "{refused}");
    assert_eq!(parse(EXAMPLE).unwrap().rulings.refusal(), None);
}

#[test]
fn pm_alone_naming_security_is_refused_at_load() {
    let security = rulings("pm_alone = [\"security\"]");
    let reason = security.refusal().expect("security is refused").to_string();
    assert!(reason.contains("`security`"), "{reason}");
}

#[test]
fn pm_alone_naming_contribution_merge_is_refused() {
    let merge = rulings("pm_alone = [\"contribution-merge\"]");
    let reason = merge.refusal().expect("contribution-merge is refused");
    assert!(
        reason.to_string().contains("`contribution-merge`"),
        "{reason}"
    );
}

#[test]
fn a_map_loads_and_a_kind_it_does_not_name_is_the_maintainer_s() {
    let map = rulings("pm_decides = { still-red = true, question = false }");
    assert!(map.pm_decides(RulingName::StillRed));
    assert!(!map.pm_decides(RulingName::Question));
    assert!(!map.pm_decides(RulingName::Rebase));
    assert_eq!(map.refusal(), None);
}

#[test]
fn an_old_list_loads_as_true_for_each_kind_it_names() {
    let list = rulings("pm_alone = [\"turn-timeout\", \"still-red\"]");
    assert!(list.pm_decides(RulingName::TurnTimeout));
    assert!(list.pm_decides(RulingName::StillRed));
    assert!(!list.pm_decides(RulingName::Rebase));
    assert_eq!(
        list.pm_decides.get(&RulingName::StillRed),
        Some(&true),
        "{list:?}"
    );
}

#[test]
fn the_map_and_the_old_list_set_at_once_are_refused_naming_both() {
    let both = parse_err(&format!(
        "{EXAMPLE}\n[app.dogs.kelpie.rulings]\npm_alone = [\"rebase\"]\n\
         pm_decides = {{ rebase = true }}\n"
    ));
    assert!(both.contains("rulings.pm_decides"), "{both}");
    assert!(both.contains("rulings.pm_alone"), "{both}");
}

#[test]
fn a_refused_kind_set_true_in_the_map_is_refused_naming_the_map() {
    for kind in [
        "merge",
        "security",
        "contribution-merge",
        "local-model-spilled",
    ] {
        let map = rulings(&format!("pm_decides = {{ {kind} = true }}"));
        let refused = map.refusal().expect("the kind is refused").to_string();
        assert!(refused.contains("rulings.pm_decides"), "{kind}: {refused}");
        assert!(refused.contains(&format!("`{kind}`")), "{kind}: {refused}");
        let off = rulings(&format!("pm_decides = {{ {kind} = false }}"));
        assert_eq!(off.refusal(), None, "{kind}");
    }
}

#[test]
fn a_kind_with_no_default_is_exactly_one_the_map_refuses() {
    for kind in RulingName::ALL {
        let set = rulings(&format!("pm_decides = {{ {} = true }}", kind.as_str()));
        assert_eq!(
            kind.pm_default().is_none(),
            set.refusal().is_some(),
            "{kind:?}"
        );
    }
    assert_eq!(RulingName::Question.pm_default(), Some(false));
    assert_eq!(RulingName::ForeignChange.pm_default(), Some(false));
    assert_eq!(RulingName::AgentFiles.pm_default(), Some(false));
    assert_eq!(RulingName::Rebase.pm_default(), Some(true));
    assert_eq!(RulingName::TurnFailed.pm_default(), Some(true));
}

// The kind after `kind` in `RulingName::ALL`. A new kind fails to compile here
// until it is threaded in, so `ALL` cannot leave it out.
fn after(kind: RulingName) -> Option<RulingName> {
    use RulingName::*;
    match kind {
        Merge => Some(Question),
        Question => Some(AgentFiles),
        AgentFiles => Some(AgentFit),
        AgentFit => Some(ForeignChange),
        ForeignChange => Some(FollowUp),
        FollowUp => Some(Rebase),
        Rebase => Some(StillRed),
        StillRed => Some(CheckQueued),
        CheckQueued => Some(NoChecks),
        NoChecks => Some(MergeRefused),
        MergeRefused => Some(Closed),
        Closed => Some(FixNotPushed),
        FixNotPushed => Some(TestStillFails),
        TestStillFails => Some(Unpushed),
        Unpushed => Some(PushRefused),
        PushRefused => Some(TurnTimeout),
        TurnTimeout => Some(TurnFailed),
        TurnFailed => Some(AdoptFailed),
        AdoptFailed => Some(OverBudget),
        OverBudget => Some(Security),
        Security => Some(ContributionMerge),
        ContributionMerge => Some(LocalModelSpilled),
        LocalModelSpilled => None,
    }
}

// `agent-fit` suggests a change to an agent file, which only the maintainer
// writes, so its kind starts the maintainer's own.
#[test]
fn agent_fit_starts_false_in_the_map() {
    assert_eq!(RulingName::AgentFit.pm_default(), Some(false));
    assert_eq!(RulingName::AgentFit.as_str(), "agent-fit");
    let map = rulings("pm_decides = { agent-fit = true }");
    assert!(map.pm_decides(RulingName::AgentFit), "the map may give it");
    assert_eq!(map.refusal(), None, "the project manager may answer it");
    assert!(
        !rulings("").pm_decides(RulingName::AgentFit),
        "an empty map"
    );
}

#[test]
fn all_names_every_kind_once() {
    let walked: Vec<RulingName> =
        std::iter::successors(Some(RulingName::Merge), |kind| after(*kind)).collect();
    assert_eq!(walked, RulingName::ALL);
}

#[test]
fn a_kind_the_map_does_not_know_is_refused_by_name() {
    let unknown = parse_err(&format!(
        "{EXAMPLE}\n[app.dogs.kelpie.rulings]\npm_decides = {{ coffee = true }}\n"
    ));
    assert!(unknown.contains("rulings.pm_decides"), "{unknown}");
    assert!(unknown.contains("coffee"), "{unknown}");
}

#[test]
fn the_issue_writers_unlabelled_kind_is_refused_by_name_in_the_map_and_the_list() {
    for lines in [
        "pm_decides = { unlabelled = true }",
        "pm_alone = [\"unlabelled\"]",
    ] {
        let err = parse_err(&format!("{EXAMPLE}\n[app.dogs.kelpie.rulings]\n{lines}\n"));
        assert!(err.contains("`unlabelled` is no longer a kind"), "{err}");
        assert!(err.contains("shep kelpie settings migrate"), "{err}");
    }
}

#[test]
fn the_panels_expert_settings_are_refused_by_name() {
    let table = "[app.dogs.kelpie.agents]\n";
    let removed = [
        ("agents.advisor", "advisor = \"advisor\"\n"),
        ("agents.researcher", "researcher = \"researcher\"\n"),
    ];
    for (key, line) in removed {
        let err = parse_err(&EXAMPLE.replace(table, &format!("{table}{line}")));
        assert!(
            err.contains(&format!("`{key}` is no longer a setting")),
            "{key}: {err}"
        );
        assert!(err.contains("shep kelpie settings migrate"), "{key}: {err}");
    }
    let research = "[app.dogs.kelpie.researcher]\nallowed_domains = [\"docs.rs\"]\n";
    let err = parse_err(&format!("{EXAMPLE}\n{research}"));
    assert!(err.contains("`researcher` is no longer a setting"), "{err}");
}
