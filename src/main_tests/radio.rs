//! The radio-drain fence (`radio_still_wanted`): a landed expansion applies
//! only while the user still wants the radio.

use crate::radio_still_wanted;
use crate::PlaySource;

#[test]
fn a_radio_source_keeps_its_result() {
    assert!(radio_still_wanted(&PlaySource::Radio(
        "yt:video:seed".into()
    )));
}

#[test]
fn any_other_source_supersedes_the_radio() {
    assert!(!radio_still_wanted(&PlaySource::Context(
        "yt:playlist:PLx".into()
    )));
    assert!(!radio_still_wanted(&PlaySource::Liked));
    assert!(!radio_still_wanted(&PlaySource::None));
}
