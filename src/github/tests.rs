use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::json;

use super::setup::{Flow, may_register, serve};
use super::tokens::{Jwt, base64url, signing_input};
use super::*;
use crate::ports::{Cost, Timestamp, Usage};
use crate::runner::step;
use crate::test::{Asked, FakeClock, FakeGithub, FakeSigner, PEM, Rig, Scripted};

// Bounds every wait on the local page, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(10);
const STATE: &str = "0123456789abcdef0123456789abcdef";

fn slug(s: &str) -> ForgeSlug {
    ForgeSlug::try_from(s.to_owned()).unwrap()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// One GET of `target` on the local page at `port`: its status and body.
fn get(port: u16, target: &str) -> (u16, String) {
    get_as(port, target, &format!("127.0.0.1:{port}"))
}

// One GET of `target` on the local page at `port`, for `host`.
fn get_as(port: u16, target: &str, host: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer[9..12].parse().unwrap();
    let body = answer.split_once("\r\n\r\n").unwrap().1.to_owned();
    (status, body)
}

// Serves the flow for `maintainer` on a free port in a thread: the port,
// and what `serve` ends with.
fn serving(home: &Path, github: &FakeGithub) -> (u16, mpsc::Receiver<std::io::Result<App>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (done, ended) = mpsc::channel();
    let (api, apps) = (github.clone(), Apps::under(home));
    std::thread::spawn(move || {
        let owner = Owner::try_from("maintainer").unwrap();
        let flow = Flow {
            name: "kelpie-maintainer",
            owner: &owner,
            org: false,
            state: STATE,
        };
        let _ = done.send(serve(&listener, &flow, &api, &apps));
    });
    (port, ended)
}

#[test]
fn a_callback_with_another_state_or_host_is_refused_and_the_right_one_keeps_the_app() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000), "Maintainer");
    let (port, ended) = serving(home.path(), &github);

    let (status, page) = get(port, "/");
    assert_eq!(status, 200);
    let action = format!("action=\"https://github.com/settings/apps/new?state={STATE}\"");
    assert!(page.contains(&action), "{page}");
    let redirect = format!("&quot;redirect_url&quot;:&quot;http://127.0.0.1:{port}/callback&quot;");
    assert!(page.contains(&redirect), "{page}");

    for wrong in [
        "/callback?code=abc123&state=0000",
        "/callback?code=abc123",
        &format!("/callback?code=abc%2F..&state={STATE}"),
    ] {
        let (status, body) = get(port, wrong);
        assert_eq!(status, 400, "{wrong}");
        assert!(body.contains("refused"), "{body}");
    }
    let right = format!("/callback?code=abc123&state={STATE}");
    for host in [
        "evil.example:80",
        &format!("127.0.0.1:{}", port + 1),
        "127.0.0.1",
    ] {
        let (status, body) = get_as(port, &right, host);
        assert_eq!(status, 400, "{host}");
        assert!(body.contains("refused"), "{body}");
    }
    assert_eq!(github.asked(), [], "a refused code is never converted");

    let (status, body) = get_as(port, &right, &format!("localhost:{port}"));
    assert_eq!(status, 200);
    assert!(
        body.contains("https://github.com/apps/kelpie-maintainer/installations/new"),
        "{body}"
    );
    let app = ended.recv_timeout(PATIENCE).unwrap().unwrap();
    assert_eq!(github.asked(), [Asked::Convert("abc123".to_owned())]);

    let owner = Owner::try_from("maintainer").unwrap();
    let kept = home.path().join("github/maintainer");
    assert_eq!(mode(&kept.join("key.pem")), 0o600);
    assert_eq!(mode(&kept), 0o700);
    assert_eq!(mode(&home.path().join("github")), 0o700);
    assert_eq!(std::fs::read_to_string(kept.join("key.pem")).unwrap(), PEM);
    let record = std::fs::read_to_string(kept.join("app.json")).unwrap();
    assert!(!record.contains("s3cr3t"), "{record}");
    assert_eq!(Apps::under(home.path()).get(&owner), Ok(Some(app.clone())));
    assert_eq!(
        app,
        App {
            id: 42,
            slug: "kelpie-maintainer".to_owned(),
            client_id: "Iv23liTEST".to_owned(),
            owner: "Maintainer".to_owned(),
        }
    );
}

#[test]
fn a_failed_conversion_or_another_owners_app_keeps_nothing_and_the_flow_waits_on() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000), "maintainer");
    let (port, ended) = serving(home.path(), &github);
    let right = format!("/callback?code=abc123&state={STATE}");

    github.fail_next(ApiError::Refused(422));
    let (status, body) = get(port, &right);
    assert_eq!(status, 502);
    assert!(body.contains("GitHub answered HTTP 422"), "{body}");

    github.converts_for("someone-else");
    let (status, body) = get(port, &right);
    assert_eq!(status, 502);
    assert!(
        body.contains("kelpie-maintainer for someone-else, not maintainer"),
        "{body}"
    );
    assert!(!home.path().join("github/someone-else").exists());
    assert!(!home.path().join("github/maintainer").exists());

    assert_eq!(get(port, &right).0, 200);
    let app = ended.recv_timeout(PATIENCE).unwrap().unwrap();
    assert_eq!(app.owner, "maintainer");
    assert_eq!(github.asked().len(), 3);
}

#[test]
fn saving_makes_the_folders_private_and_refuses_a_link() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(0), "shep-pm");
    let folder = home.path().join("github/shep-pm");
    std::fs::create_dir_all(&folder).unwrap();
    for open in [home.path().join("github"), folder.clone()] {
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let apps = Apps::under(home.path());

    let app = apps.save(&github.conversion()).unwrap();

    assert_eq!(mode(&folder), 0o700);
    assert_eq!(mode(&home.path().join("github")), 0o700);
    let owner = Owner::try_from("shep-pm").unwrap();
    assert_eq!(apps.get(&owner), Ok(Some(app)));

    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), &folder).unwrap();
    assert_eq!(
        apps.save(&github.conversion()),
        Err(StoreError::Exposed(folder))
    );
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn an_org_s_manifest_goes_to_the_org_s_new_app_page() {
    let org = Owner::try_from("shep-pm").unwrap();
    let flow = Flow {
        name: "kelpie-shep-pm",
        owner: &org,
        org: true,
        state: STATE,
    };
    assert_eq!(
        flow.new_app_url(),
        format!("https://github.com/organizations/shep-pm/settings/apps/new?state={STATE}")
    );
    let manifest: serde_json::Value = serde_json::from_str(&flow.manifest(4000)).unwrap();
    assert_eq!(
        manifest,
        json!({
            "name": "kelpie-shep-pm",
            "url": "https://github.com/shep-pm/shep-kelpie",
            "redirect_url": "http://127.0.0.1:4000/callback",
            "public": false,
            "hook_attributes": { "url": "https://github.com/shep-pm/shep-kelpie", "active": false },
            "default_permissions": {
                "issues": "write",
                "pull_requests": "write",
                "metadata": "read",
            },
        })
    );
}

#[test]
fn setup_again_for_an_owner_with_an_app_stops_unless_replacing() {
    let home = tempfile::tempdir().unwrap();
    let apps = Apps::under(home.path());
    let owner = Owner::try_from("shep-pm").unwrap();
    assert_eq!(may_register(&apps, &owner, false), Ok(None));
    FakeGithub::new(FakeClock::at(0), "shep-pm").registered(home.path(), "shep-pm/koji");

    let why = may_register(&apps, &owner, false).unwrap_err();

    assert!(
        why.contains("already has a GitHub App for shep-pm, kelpie-shep-pm"),
        "{why}"
    );
    assert!(why.contains("`--replace`"), "{why}");
    assert!(why.contains("delete kelpie-shep-pm"), "{why}");
    assert!(why.contains("`--name`"), "{why}");
    assert_eq!(may_register(&apps, &owner, true), Ok(None));

    let key = home.path().join("github/shep-pm/key.pem");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let warning = may_register(&apps, &owner, true).unwrap().unwrap();
    assert!(warning.starts_with("warning: "), "{warning}");
    assert!(
        warning.contains("key.pem may be used by others"),
        "{warning}"
    );
}

#[test]
fn a_token_is_kept_until_five_minutes_before_it_lapses_then_minted_again() {
    let home = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let github = FakeGithub::new(clock.clone(), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    let tokens = github.tokens(home.path());
    let koji = slug("shep-pm/koji");

    let first = tokens.token(&koji).unwrap();
    clock.advance(FakeGithub::LIFETIME - 5 * 60 - 1);
    assert_eq!(tokens.token(&koji).unwrap(), first, "still minutes to run");
    assert_eq!(
        github.asked(),
        [
            Asked::Installation("shep-pm/koji".to_owned()),
            Asked::AccessToken(7, r#"{"repositories":["koji"]}"#.to_owned())
        ]
    );

    clock.advance(1);
    let second = tokens.token(&koji).unwrap();

    assert_ne!(second, first);
    assert_eq!(second.expose(), "ghs_test2");
    assert_eq!(
        github.asked().last(),
        Some(&Asked::AccessToken(
            7,
            r#"{"repositories":["koji"]}"#.to_owned()
        )),
        "the installation is known, so only a token is asked for"
    );
    assert_eq!(github.asked().len(), 3);
}

#[test]
fn each_repo_gets_a_token_of_its_own_and_a_lost_installation_is_asked_for_again() {
    let home = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let github = FakeGithub::new(clock.clone(), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    github.install("shep-pm/golbat", 7);
    let tokens = github.tokens(home.path());

    let koji = tokens.token(&slug("shep-pm/koji")).unwrap();
    let golbat = tokens.token(&slug("shep-pm/golbat")).unwrap();

    assert_ne!(koji, golbat, "one installation, a token per repo");
    let body = |name: &str| format!(r#"{{"repositories":["{name}"]}}"#);
    assert_eq!(github.asked()[3], Asked::AccessToken(7, body("golbat")));

    clock.advance(FakeGithub::LIFETIME);
    github.fail_next(ApiError::Refused(404));
    let err = tokens.token(&slug("shep-pm/koji")).unwrap_err();
    assert_eq!(err, TokenError::Api(ApiError::Refused(404)));
    tokens.token(&slug("shep-pm/koji")).unwrap();
    assert_eq!(
        github.asked()[5..],
        [
            Asked::Installation("shep-pm/koji".to_owned()),
            Asked::AccessToken(7, body("koji"))
        ],
        "the installation was dropped and asked for again"
    );
}

#[test]
fn an_owner_that_is_no_login_is_refused_before_any_file_is_read() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(0), "shep-pm");
    let err = github
        .tokens(home.path())
        .token(&slug("../koji"))
        .unwrap_err();
    assert_eq!(err.to_string(), r#"".." is not a GitHub login"#);
}

#[test]
fn a_redirect_or_a_rate_limit_is_told_from_a_refusal() {
    assert_eq!(api::refusal(301, ""), ApiError::Moved);
    assert!(ApiError::Moved.to_string().contains("`git.remote`"));
    assert_eq!(api::refusal(429, ""), ApiError::RateLimited);
    let limited = r#"{"message":"API rate limit exceeded for installation ID 7."}"#;
    assert_eq!(api::refusal(403, limited), ApiError::RateLimited);
    let denied = r#"{"message":"Resource not accessible by integration"}"#;
    assert_eq!(api::refusal(403, denied), ApiError::Refused(403));
    assert!(ApiError::Refused(502).passes() && ApiError::RateLimited.passes());
    assert!(!ApiError::Refused(401).passes() && !ApiError::Moved.passes());
    assert_eq!(
        api::token_request(&slug("shep-pm/koji")),
        r#"{"repositories":["koji"]}"#
    );
}

#[test]
fn the_jwt_is_the_apps_client_id_issued_a_minute_back_for_nine_minutes() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000_000), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");

    github
        .tokens(home.path())
        .token(&slug("shep-pm/koji"))
        .unwrap();

    let claims = r#"{"exp":1000540,"iat":999940,"iss":"Iv23liTEST"}"#;
    let expected = format!(
        "{}.{}.{}",
        base64url(br#"{"alg":"RS256","typ":"JWT"}"#),
        base64url(claims.as_bytes()),
        base64url(crate::test::FakeSigner::SIGNATURE)
    );
    assert_eq!(github.jwts(), [expected.clone(), expected.clone()]);
    assert_eq!(
        signing_input("Iv23liTEST", Timestamp(1_000_000)),
        expected.rsplit_once('.').unwrap().0
    );
}

#[test]
fn no_app_or_no_install_says_which() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(0), "shep-pm");
    let tokens = github.tokens(home.path());
    let owner = Owner::try_from("shep-pm").unwrap();
    assert_eq!(
        tokens.token(&slug("shep-pm/koji")),
        Err(TokenError::NoApp(owner))
    );

    github.registered(home.path(), "shep-pm/koji");
    let err = tokens.token(&slug("Shep-PM/golbat")).unwrap_err();

    assert_eq!(
        err.to_string(),
        "kelpie-shep-pm is not installed on Shep-PM/golbat"
    );
    tokens.token(&slug("Shep-PM/Koji")).unwrap();
}

#[test]
fn a_key_others_may_read_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(0), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    let key = home.path().join("github/shep-pm/key.pem");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

    let err = github
        .tokens(home.path())
        .token(&slug("shep-pm/koji"))
        .unwrap_err();

    assert_eq!(err, TokenError::Store(StoreError::Exposed(key)));
    assert_eq!(github.asked(), [], "nothing is signed with an exposed key");
}

// From the example answers in GitHub's REST documentation for each call.
#[test]
fn githubs_answers_are_read() {
    let conversion = r#"{"id":1,"slug":"octoapp","node_id":"MDxOkludGVncmF0aW9uMQ==",
        "owner":{"login":"github","id":1,"type":"Organization"},"name":"Octocat App",
        "client_id":"Iv1.8a61f9b3a7aba766","client_secret":"1726be1638095a19edd134c77bde3aa2ece1e5d8",
        "webhook_secret":"e340154128314309424b7c8e90325147d99fdafa",
        "pem":"-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\n"}"#;
    let read = api::parse_conversion(conversion).unwrap();
    assert_eq!(
        (read.id, read.slug.as_str(), read.owner.as_str()),
        (1, "octoapp", "github")
    );
    assert_eq!(read.client_id, "Iv1.8a61f9b3a7aba766");
    assert!(
        read.pem
            .expose()
            .starts_with("-----BEGIN RSA PRIVATE KEY-----\n")
    );

    let installation =
        r#"{"id":1,"account":{"login":"octocat"},"app_id":1,"target_type":"Organization"}"#;
    assert_eq!(api::parse_installation(installation), Ok(1));

    let token = r#"{"token":"ghs_16C7e42F292c6912E7710c838347Ae178B4a","expires_at":"2016-07-11T22:14:10Z",
        "permissions":{"issues":"write"},"repository_selection":"selected"}"#;
    let issued = api::parse_token(token).unwrap();
    assert_eq!(
        issued.token.expose(),
        "ghs_16C7e42F292c6912E7710c838347Ae178B4a"
    );
    assert_eq!(issued.expires_at, Timestamp(1_468_275_250));
    assert_eq!(
        api::parse_token(r#"{"token":"t"}"#),
        Err(ApiError::Unreadable("expires_at"))
    );
}

// A lazy `derive(Debug)` would print the key or a token into a log.
#[test]
fn debug_never_shows_a_key_a_jwt_or_a_token() {
    let conversion = FakeGithub::new(FakeClock::at(0), "o").conversion();
    let shown = format!("{conversion:?}");
    assert!(shown.contains("pem: Pem(..)"), "{shown}");
    assert!(!shown.contains("s3cr3t"), "{shown}");
    let token = InstallationToken::new("ghs_secret".to_owned());
    assert_eq!(format!("{token:?}"), "InstallationToken(..)");
}

#[test]
fn base64url_follows_rfc_4648_without_padding() {
    for (plain, encoded) in [
        (&b""[..], ""),
        (b"f", "Zg"),
        (b"fo", "Zm8"),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg"),
        (b"fooba", "Zm9vYmE"),
        (b"foobar", "Zm9vYmFy"),
        (&[0xfb, 0xff], "-_8"),
    ] {
        assert_eq!(base64url(plain), encoded);
    }
}

// The real signer, against a key `openssl` makes here: never GitHub.
#[test]
fn openssl_signs_what_its_public_key_verifies() {
    use crate::github::Signer as _;
    let dir = tempfile::tempdir().unwrap();
    let [key, public, sig, input] =
        ["key.pem", "pub.pem", "sig", "input"].map(|f| dir.path().join(f));
    let openssl = |args: &[&std::ffi::OsStr]| {
        let ran = std::process::Command::new("openssl")
            .args(args)
            .output()
            .unwrap();
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        String::from_utf8_lossy(&ran.stdout).into_owned()
    };
    openssl(&[
        "genrsa".as_ref(),
        "-out".as_ref(),
        key.as_os_str(),
        "2048".as_ref(),
    ]);
    openssl(&[
        "rsa".as_ref(),
        "-in".as_ref(),
        key.as_os_str(),
        "-pubout".as_ref(),
        "-out".as_ref(),
        public.as_os_str(),
    ]);
    let signed = signing_input("Iv23liTEST", Timestamp(1_000_000));

    let signature = crate::adapters::Openssl
        .sign(&key, signed.as_bytes())
        .unwrap();

    std::fs::write(&sig, signature).unwrap();
    std::fs::write(&input, &signed).unwrap();
    let verified = openssl(&[
        "dgst".as_ref(),
        "-sha256".as_ref(),
        "-verify".as_ref(),
        public.as_os_str(),
        "-signature".as_ref(),
        sig.as_os_str(),
        input.as_os_str(),
    ]);
    assert!(verified.contains("Verified OK"), "{verified}");
}

// The App's key sits in kelpie's home, which a worker's sandbox denies
// around its own folders; it never enters a variable or the worktree.
#[test]
fn the_apps_key_never_reaches_a_worker() {
    let rig = Rig::new("koji");
    let kelpie = rig.home.path().join("shep/kelpie");
    FakeGithub::new(rig.clock.clone(), "shep-pm").registered(&kelpie, "shep-pm/koji");
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "add", Some("7"))["work_item"]["issue"], 7);
    let usage = Usage {
        input: 1,
        cache_write: 0,
        cache_write_5m: 0,
        cache_read: 0,
        output: 1,
    };
    rig.claude.script([Scripted::Reply(usage, Cost(1))]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();

    let key = "kelpie-test-app-key-s3cr3t";
    assert!(!seen.settings.to_string().contains(key));
    assert!(!seen.sandbox.to_string().contains(key));
    let fence = seen
        .call
        .reach
        .fence
        .as_deref()
        .expect("a worker is fenced");
    assert!(
        fence
            .env
            .values()
            .all(|v| !v.to_string_lossy().contains(key))
    );
    let shep = rig.home.path().join("shep");
    let denied = seen.sandbox["filesystem"]["denyRead"].as_array().unwrap();
    assert!(
        denied.contains(&json!(format!("{}/**", shep.display()))),
        "{denied:?}"
    );
    let allowed = seen.sandbox["filesystem"]["allowRead"].to_string();
    assert!(!allowed.contains("github"), "{allowed}");
    let deny = seen.settings["permissions"]["deny"].to_string();
    let rule = format!("Read(/{}/**)", kelpie.join(FOLDER).display());
    assert!(deny.contains(&rule), "{rule} not in {deny}");

    let mut folders = vec![seen.call.cwd.clone()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if let Ok(text) = std::fs::read(&path) {
                assert!(!String::from_utf8_lossy(&text).contains(key), "{path:?}");
            }
        }
    }
}

// openssl would wait forever on a FIFO's open, and doctor with it.
#[test]
fn a_key_that_is_no_regular_file_is_refused_before_it_is_signed_with() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(0), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    let key = home.path().join("github/shep-pm/key.pem");
    std::fs::remove_file(&key).unwrap();
    let owner_only = nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR;
    nix::unistd::mkfifo(&key, owner_only).unwrap();

    let err = github
        .tokens(home.path())
        .token(&slug("shep-pm/koji"))
        .unwrap_err();

    let TokenError::Store(StoreError::Unusable(why)) = err else {
        panic!("{err:?}");
    };
    assert!(why.contains("key.pem is not a regular file"), "{why}");
    assert_eq!(github.asked(), []);
}

#[test]
fn a_replaced_app_never_hands_out_the_old_apps_token() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000_000), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    let tokens = github.tokens(home.path());
    let koji = slug("shep-pm/koji");
    let old = tokens.token(&koji).unwrap();

    let replaced = Conversion {
        id: 43,
        ..github.conversion()
    };
    Apps::under(home.path()).save(&replaced).unwrap();
    let new = tokens.token(&koji).unwrap();

    assert_ne!(new, old);
    assert_eq!(
        github.asked()[2],
        Asked::Installation("shep-pm/koji".to_owned()),
        "the new App's installation is asked for afresh"
    );
}

// A GitHub whose answer about one repo's installation waits to be let go.
struct Stalled {
    inner: FakeGithub,
    repo: &'static str,
    inside: mpsc::Sender<()>,
    release: std::sync::Mutex<mpsc::Receiver<()>>,
}

impl GithubApi for Stalled {
    fn convert(&self, code: &str) -> Result<Conversion, ApiError> {
        self.inner.convert(code)
    }

    fn installation(&self, jwt: &Jwt, repo: &ForgeSlug) -> Result<Option<u64>, ApiError> {
        if repo.as_str() == self.repo {
            self.inside.send(()).unwrap();
            let _ = self.release.lock().unwrap().recv_timeout(PATIENCE);
        }
        self.inner.installation(jwt, repo)
    }

    fn access_token(
        &self,
        jwt: &Jwt,
        installation: u64,
        repo: &ForgeSlug,
    ) -> Result<IssuedToken, ApiError> {
        self.inner.access_token(jwt, installation, repo)
    }

    fn write(&self, token: &InstallationToken, call: &Call) -> Result<String, ApiError> {
        self.inner.write(token, call)
    }
}

#[test]
fn a_slow_answer_for_one_repo_stalls_no_other_repo_s_token() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000_000), "shep-pm");
    github.registered(home.path(), "shep-pm/slow");
    github.install("shep-pm/quick", 7);
    let (inside, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let stalled = Stalled {
        inner: github.clone(),
        repo: "shep-pm/slow",
        inside,
        release: std::sync::Mutex::new(released),
    };
    let tokens = Arc::new(AppTokens::new(
        Apps::under(home.path()),
        Box::new(stalled),
        Box::new(FakeSigner),
        Box::new(FakeClock::at(1_000_000)),
    ));
    let slow = {
        let tokens = Arc::clone(&tokens);
        std::thread::spawn(move || tokens.token(&slug("shep-pm/slow")))
    };
    entered.recv_timeout(PATIENCE).unwrap();

    let (done, minted) = mpsc::channel();
    let quick = {
        let tokens = Arc::clone(&tokens);
        std::thread::spawn(move || done.send(tokens.token(&slug("shep-pm/quick"))))
    };
    let quick_token = minted.recv_timeout(Duration::from_secs(2));
    release.send(()).unwrap();

    assert!(
        quick_token.is_ok_and(|token| token.is_ok()),
        "another repo's token waits on the slow one's answer"
    );
    quick.join().unwrap().unwrap();
    slow.join().unwrap().unwrap();
}

#[test]
fn askers_for_one_repo_at_once_mint_one_token() {
    let home = tempfile::tempdir().unwrap();
    let github = FakeGithub::new(FakeClock::at(1_000_000), "shep-pm");
    github.registered(home.path(), "shep-pm/koji");
    let tokens = Arc::new(github.tokens(home.path()));
    let askers: Vec<_> = (0..4)
        .map(|_| {
            let tokens = Arc::clone(&tokens);
            std::thread::spawn(move || tokens.token(&slug("shep-pm/koji")).unwrap())
        })
        .collect();
    let got: Vec<_> = askers.into_iter().map(|t| t.join().unwrap()).collect();

    assert!(got.iter().all(|t| *t == got[0]));
    let minted = (github.asked().iter())
        .filter(|a| matches!(a, Asked::AccessToken(..)))
        .count();
    assert_eq!(minted, 1);
}
