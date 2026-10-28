use super::*;
use crate::lease::door_pace::DoorPace;
use crate::ports::{Pace, Pass, Timestamp};

// The dog's clock is the book's, fixed at `EPOCH` in these tests.
const NOW: Timestamp = Timestamp(EPOCH);

#[tokio::test]
async fn a_hold_one_runner_reports_stops_the_other_runner_on_the_same_door() {
    let (_dir, socket, _desk) = door(3);
    let (koji, golbat) = (DoorPace::new(socket.clone()), DoorPace::new(socket));

    let seen = off_the_runtime(move || {
        koji.hold(Timestamp(NOW.0 + 90));
        (golbat.pass("golbat", NOW), golbat.held(NOW))
    })
    .await;

    assert_eq!(
        seen,
        (
            Pass::Held(Timestamp(NOW.0 + 90)),
            Some(Timestamp(NOW.0 + 90))
        )
    );
}

#[tokio::test]
async fn runners_on_one_door_are_let_go_when_nothing_holds_them() {
    let (_dir, socket, _desk) = door(3);
    let koji = DoorPace::new(socket);

    let seen = off_the_runtime(move || (koji.pass("koji", NOW), koji.held(NOW))).await;

    assert_eq!(seen, (Pass::Go, None));
}
