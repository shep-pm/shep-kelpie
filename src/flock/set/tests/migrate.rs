//! `settings migrate` against the stand-in shepherd's flock

use super::*;
use crate::settings::RulingName;

impl Scene {
    async fn migrate(&self) -> Result<Vec<String>, String> {
        let client = self.shepherd.connect().await;
        let home = self.shepherd.scratch("home");
        let kelpie_home = home.join("kelpie");
        let homes = Homes {
            home: &home,
            kelpie_home: &kelpie_home,
            shep_home: self.shepherd.home(),
        };
        let done = crate::flock::set::migrate::migrate(&client, homes);
        tokio::time::timeout(CALL_PATIENCE, done)
            .await
            .expect("migrate neither ended nor failed in time")
    }
}

fn with_rulings(lines: &str) -> String {
    format!("{TABLE}\n[rulings]\n{lines}\n")
}

fn map_of(table: &Map<String, Value>) -> &Map<String, Value> {
    table["rulings"]["pm_decides"]
        .as_object()
        .expect("the map is written")
}

#[tokio::test]
async fn an_empty_rulings_table_gets_every_kind_that_has_a_default() {
    let scene = Scene::holding(&with_rulings("")).await;

    let said = scene.migrate().await.unwrap();

    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].starts_with("project eevee: wrote "), "{said:?}");
    assert!(said[1].starts_with("project koji: wrote "), "{said:?}");
    assert!(
        said[1].contains("rulings.pm_decides.rebase = true"),
        "{said:?}"
    );
    let table = scene.table();
    let map = map_of(&table);
    for kind in RulingName::ALL {
        let written = map.get(kind.as_str()).and_then(Value::as_bool);
        assert_eq!(written, kind.pm_default(), "{kind:?}");
    }
}

#[tokio::test]
async fn a_value_already_there_is_never_changed() {
    let table = with_rulings("pm_decides = { question = true, rebase = false }");
    let scene = Scene::holding(&table).await;

    scene.migrate().await.unwrap();

    let table = scene.table();
    let map = map_of(&table);
    assert_eq!(map["question"], true);
    assert_eq!(map["rebase"], false);
    assert_eq!(map["still-red"], true);
    assert_eq!(map["agent-files"], false);
}

#[tokio::test]
async fn an_old_list_becomes_entries_in_the_map_and_is_removed() {
    let scene = Scene::holding(&with_rulings("pm_alone = [\"question\"]")).await;

    let said = scene.migrate().await.unwrap();

    assert!(said[1].contains("rulings.pm_alone"), "{said:?}");
    let table = scene.table();
    assert!(table["rulings"].get("pm_alone").is_none(), "{table:?}");
    let map = map_of(&table);
    assert_eq!(map["question"], true);
    assert_eq!(map["foreign-change"], false);
    assert_eq!(map["turn-failed"], true);
}

#[tokio::test]
async fn a_second_run_has_nothing_to_migrate() {
    let mut scene = Scene::new().await;
    scene.migrate().await.unwrap();
    let after = scene.table();
    assert_eq!(scene.shepherd.writes().len(), 2);

    let said = scene.migrate().await.unwrap();

    assert_eq!(said, ["nothing to migrate"]);
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.table(), after);
}

#[tokio::test]
async fn a_table_that_would_not_load_is_not_written_and_the_others_are() {
    let mut scene =
        Scene::holding_each(&with_rulings("pm_alone = [\"merge\"]"), &with_rulings("")).await;
    let before = scene.table();

    let refused = scene.migrate().await.unwrap_err();

    assert!(refused.contains("project koji"), "{refused}");
    assert!(refused.contains("rulings.pm_decides"), "{refused}");
    assert!(refused.contains("project eevee: wrote "), "{refused}");
    assert_eq!(scene.table(), before);
    assert_eq!(scene.shepherd.writes().len(), 1);
    assert!(map_of(&scene.table_of("eevee")).contains_key("rebase"));
}

#[tokio::test]
async fn a_list_with_an_entry_that_is_no_kind_s_name_is_refused_by_name_and_kept() {
    let mut scene = Scene::holding_each(
        &with_rulings("pm_alone = [\"rebase\", 3]"),
        &with_rulings(""),
    )
    .await;
    let before = scene.table();

    let refused = scene.migrate().await.unwrap_err();

    assert!(refused.contains("project koji: not migrated"), "{refused}");
    assert!(refused.contains("rulings.pm_alone"), "{refused}");
    assert_eq!(scene.table(), before);
    assert_eq!(scene.shepherd.writes().len(), 1);
}

#[tokio::test]
async fn the_panels_keys_are_deleted_and_named_and_the_table_loads() {
    let panel = "decide = \"panel\"\nfull_panel_paths = [\"src/**\"]";
    let scene = Scene::holding(&with_rulings(panel)).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[1].contains(
            "removed rulings.decide = \"panel\", rulings.full_panel_paths = [\"src/**\"]"
        ),
        "{said:?}"
    );
    let table = scene.table();
    for key in ["decide", "full_panel_paths"] {
        assert!(table["rulings"].get(key).is_none(), "{table:?}");
    }
}

#[tokio::test]
async fn a_table_whose_map_is_whole_has_the_panels_key_deleted_alone() {
    let whole: Vec<String> = (RulingName::ALL.into_iter())
        .filter_map(|kind| Some(format!("\"{}\" = {}", kind.as_str(), kind.pm_default()?)))
        .collect();
    let lines = format!(
        "decide = \"shadow\"\npm_decides = {{ {} }}",
        whole.join(", ")
    );
    let table = TABLE.replace("reviewers = [\"standards\"]\n", "");
    let scene = Scene::holding(&format!("{table}\n[rulings]\n{lines}\n")).await;

    let said = scene.migrate().await.unwrap();

    assert_eq!(
        said[1], "project koji: removed rulings.decide = \"shadow\"",
        "{said:?}"
    );
    assert!(scene.table()["rulings"].get("decide").is_none());
}

#[tokio::test]
async fn the_panels_expert_keys_and_roles_are_deleted_and_named_and_the_table_loads() {
    let experts = TABLE.replace(
        "[agents]\n",
        "[agents]\nadvisor = \"advisor\"\nresearcher = \"researcher\"\n",
    );
    let lines = "[researcher]\nallowed_domains = [\"docs.rs\"]\n\n\
                 [repo_setup]\nroles = [\"worker\", \"advisor\", \"researcher\"]\n";
    let scene = Scene::holding(&format!("{experts}\n{lines}")).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[1].contains(
            "removed agents.advisor = \"advisor\", agents.researcher = \"researcher\", \
             researcher = { allowed_domains = [\"docs.rs\"] }, \
             agents.reviewers = [\"standards\"], \
             advisor from repo_setup.roles, researcher from repo_setup.roles"
        ),
        "{said:?}"
    );
    let table = scene.table();
    assert!(table["agents"].get("advisor").is_none(), "{table:?}");
    assert!(table["agents"].get("researcher").is_none(), "{table:?}");
    assert!(table.get("researcher").is_none(), "{table:?}");
    assert_eq!(table["repo_setup"]["roles"], serde_json::json!(["worker"]));
}

#[tokio::test]
async fn the_keys_the_agent_board_replaced_are_deleted_and_named_and_the_rest_is_kept() {
    let old = TABLE.replace(
        "[agents]\n",
        "[agents]\nimplementers = [\"sonnet-high\", \"opus-high\"]\n\
         implementer_choice = \"issue-writer\"\nfallback_after = 15\n",
    );
    let scene = Scene::holding_each(&old, TABLE).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[1].contains(
            "removed agents.implementers = [\"sonnet-high\", \"opus-high\"], \
             agents.implementer_choice = \"issue-writer\", agents.fallback_after = 15"
        ),
        "{said:?}"
    );
    // Only the checkout and the repo tell the two projects apart.
    let rest = |mut table: Map<String, Value>| {
        table.remove("git");
        table
    };
    assert_eq!(
        rest(scene.table()),
        rest(scene.table_of("eevee")),
        "every other key is as it was"
    );
}

#[tokio::test]
async fn the_issue_writers_unlabelled_kind_is_deleted_from_the_map_and_the_list() {
    let map = with_rulings("pm_decides = { unlabelled = true, rebase = false }");
    let list = with_rulings("pm_alone = [\"unlabelled\", \"question\"]");
    let scene = Scene::holding_each(&map, &list).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[0].contains("unlabelled from rulings.pm_alone"),
        "{said:?}"
    );
    assert!(
        said[1].contains("rulings.pm_decides.unlabelled"),
        "{said:?}"
    );
    for table in [scene.table_of("eevee"), scene.table()] {
        let map = map_of(&table);
        assert!(map.get("unlabelled").is_none(), "{map:?}");
        assert_eq!(map.len(), RulingName::ALL.len() - 4, "{map:?}");
    }
}

#[tokio::test]
async fn the_issue_writers_role_is_deleted_from_repo_setup_and_the_table_loads() {
    let lines = "[repo_setup]\nroles = [\"worker\", \"issue-writer\", \"pm\"]\n";
    let scene = Scene::holding(&format!("{TABLE}\n{lines}")).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[1].contains(
            "removed agents.reviewers = [\"standards\"], issue-writer from repo_setup.roles"
        ),
        "{said:?}"
    );
    let roles = &scene.table()["repo_setup"]["roles"];
    assert_eq!(*roles, serde_json::json!(["worker", "pm"]));
}

#[tokio::test]
async fn the_issue_writers_agent_key_is_deleted_and_named_and_the_table_loads() {
    let named = TABLE.replace("[agents]\n", "[agents]\nissue_writer = \"scribe\"\n");
    let scene = Scene::holding(&named).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[1].contains("removed agents.issue_writer = \"scribe\""),
        "{said:?}"
    );
    assert!(scene.table()["agents"].get("issue_writer").is_none());
}

// `TABLE` with `lines` in its `agents` table in place of its reviewers
fn with_agents(lines: &str) -> String {
    TABLE.replace("reviewers = [\"standards\"]", lines)
}

fn agents_of(table: &Map<String, Value>) -> &Map<String, Value> {
    table["agents"]
        .as_object()
        .expect("the agents table is there")
}

#[tokio::test]
async fn a_reviewers_list_joins_a_listed_allow_and_is_deleted() {
    let table = with_agents("allow = [\"sonnet-high\"]\nreviewers = [\"standards\", \"spec\"]");
    let mut scene = Scene::holding(&table).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[1].contains(
            "agents.allow = [\"sonnet-high\", \"standards\", \"spec\"]; \
             removed agents.reviewers = [\"standards\", \"spec\"]"
        ),
        "{said:?}"
    );
    let read = scene.table();
    let agents = agents_of(&read);
    assert_eq!(agents.get("reviewers"), None, "{agents:?}");
    assert_eq!(
        agents["allow"],
        serde_json::json!(["sonnet-high", "standards", "spec"])
    );

    assert_eq!(scene.shepherd.writes().len(), 2, "one write per project");

    let said = scene.migrate().await.unwrap();
    assert_eq!(said, ["nothing to migrate"], "a second run writes nothing");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn with_no_allow_a_reviewers_list_is_deleted_and_no_allow_is_written() {
    // eevee's `allow` is an empty list, which allows every agent as an absent one does
    let empty = with_agents("allow = []\nreviewers = [\"standards\"]");
    let scene = Scene::holding_each(TABLE, &empty).await;

    let said = scene.migrate().await.unwrap();

    assert!(
        said[0].contains("removed agents.reviewers = [\"standards\"]"),
        "{said:?}"
    );
    assert!(!said[0].contains("wrote agents.allow"), "{said:?}");
    let (eevee_table, koji_table) = (scene.table_of("eevee"), scene.table());
    let (eevee, koji) = (agents_of(&eevee_table), agents_of(&koji_table));
    assert_eq!(koji.get("reviewers"), None, "{koji:?}");
    assert_eq!(koji.get("allow"), None, "an absent allow stays absent");
    assert_eq!(eevee.get("reviewers"), None, "{eevee:?}");
    assert_eq!(
        eevee["allow"],
        serde_json::json!([]),
        "an empty one stays empty"
    );
}

#[tokio::test]
async fn a_reviewer_already_allowed_is_not_written_twice() {
    let table = with_agents(
        "allow = [\"standards\", \"opus-high\"]\nreviewers = [\"standards\", \"spec\"]",
    );
    let scene = Scene::holding(&table).await;

    scene.migrate().await.unwrap();

    let read = scene.table();
    let agents = agents_of(&read);
    assert_eq!(
        agents["allow"],
        serde_json::json!(["standards", "opus-high", "spec"]),
        "each name once, in the order the lists held them"
    );
}
