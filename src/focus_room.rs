use serde::{Deserialize, Serialize};

/// Server-owned session identity for the persistent Focus Room.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FocusRoom {
    pub(crate) tab_id: Option<String>,
    pub(crate) active: bool,
    pub(crate) return_to: Option<String>,
}

impl FocusRoom {
    pub(crate) fn enter(&mut self, current_tab_id: Option<String>) {
        if !self.active {
            if let Some(tab_id) =
                current_tab_id.filter(|tab| self.tab_id.as_deref() != Some(tab.as_str()))
            {
                self.return_to = Some(tab_id);
            }
        }
        self.active = true;
    }

    pub(crate) fn leave(&mut self) -> Option<&str> {
        self.active = false;
        self.return_to.as_deref()
    }

    pub(crate) fn tab_disappeared(&mut self) {
        self.active = false;
        self.tab_id = None;
    }

    pub(crate) fn snapshot(&self) -> Option<FocusRoomSnapshot> {
        (self.tab_id.is_some() || self.active || self.return_to.is_some()).then(|| {
            FocusRoomSnapshot {
                tab_id: self.tab_id.clone(),
                active: self.active,
                return_to: self.return_to.clone(),
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FocusRoomSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tab_id: Option<String>,
    #[serde(default)]
    pub(crate) active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) return_to: Option<String>,
}

impl From<FocusRoomSnapshot> for FocusRoom {
    fn from(snapshot: FocusRoomSnapshot) -> Self {
        Self {
            tab_id: snapshot.tab_id,
            active: snapshot.active,
            return_to: snapshot.return_to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_leave_and_reenter_preserve_the_return_tab() {
        let mut room = FocusRoom {
            tab_id: Some("focus-tab".into()),
            ..FocusRoom::default()
        };
        room.enter(Some("work-tab".into()));
        assert!(room.active);
        assert_eq!(room.return_to.as_deref(), Some("work-tab"));
        assert_eq!(room.leave(), Some("work-tab"));
        room.enter(Some("focus-tab".into()));
        assert!(room.active);
        assert_eq!(room.return_to.as_deref(), Some("work-tab"));
    }

    #[test]
    fn entering_from_the_room_does_not_return_to_itself() {
        let mut room = FocusRoom {
            tab_id: Some("focus-tab".into()),
            active: false,
            return_to: None,
        };
        room.enter(Some("focus-tab".into()));
        assert_eq!(room.return_to, None);
    }

    #[test]
    fn a_disappearing_room_clears_its_identity() {
        let mut room = FocusRoom {
            tab_id: Some("focus-tab".into()),
            active: true,
            return_to: Some("work-tab".into()),
        };
        room.tab_disappeared();
        assert_eq!(room.tab_id, None);
        assert!(!room.active);
        assert_eq!(room.return_to.as_deref(), Some("work-tab"));
    }

    #[test]
    fn snapshot_roundtrips_and_absence_is_backward_compatible() {
        let room = FocusRoom {
            tab_id: Some("focus-tab".into()),
            active: true,
            return_to: Some("work-tab".into()),
        };
        let encoded = serde_json::to_string(&room.snapshot()).unwrap();
        let decoded: Option<FocusRoomSnapshot> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.map(FocusRoom::from), Some(room));
        let old_snapshot: Option<FocusRoomSnapshot> = serde_json::from_str("null").unwrap();
        assert_eq!(
            old_snapshot.map(FocusRoom::from).unwrap_or_default(),
            FocusRoom::default()
        );
    }
}
