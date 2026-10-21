//! Who may change fans (DESIGN.md §7.4). The socket is world-connectable; every connection's peer credentials decide once
//! whether its writes are allowed. Reads are open to any local user.

/// The connecting process, from `SO_PEERCRED` and `/proc/<pid>/status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub uid: u32,
    pub gid: u32,
    /// Supplementary groups.
    pub groups: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The daemon's own uid: under `--mock` it may write too, so a person can drive the mock desk without a group.
    pub daemon_uid: u32,
    pub mock: bool,
    /// The `penna` group's gid, if the group exists.
    pub penna_gid: Option<u32>,
}

/// `Ok` when the peer may write; otherwise why not, as a sentence that says what to do.
pub fn decide(peer: &Peer, policy: &Policy) -> Result<(), String> {
    if peer.uid == 0 || (policy.mock && peer.uid == policy.daemon_uid) {
        return Ok(());
    }
    match policy.penna_gid {
        Some(g) if peer.gid == g || peer.groups.contains(&g) => Ok(()),
        Some(_) => Err("Changing fans needs membership of the penna group: run sudo usermod -aG penna $USER, then log in again.".into()),
        None => Err("Changing fans needs membership of the penna group, which doesn't exist yet: installing Penna creates it.".into()),
    }
}

/// The `penna` group's gid from `/etc/group`.
pub fn penna_gid() -> Option<u32> {
    let text = std::fs::read_to_string("/etc/group").ok()?;
    group_gid(&text, "penna")
}

fn group_gid(etc_group: &str, name: &str) -> Option<u32> {
    etc_group.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then_some(())?;
        f.nth(1)?.parse().ok()
    })
}

/// A process's supplementary groups, from the `Groups:` line of `/proc/<pid>/status`.
pub fn groups_of(pid: i32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status")).map(|s| parse_groups(&s)).unwrap_or_default()
}

fn parse_groups(status: &str) -> Vec<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Groups:"))
        .map(|g| g.split_whitespace().filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uid: u32, gid: u32, groups: &[u32]) -> Peer {
        Peer { uid, gid, groups: groups.to_vec() }
    }

    #[test]
    fn root_and_the_penna_group_may_write() {
        let p = Policy { daemon_uid: 0, mock: false, penna_gid: Some(960) };
        assert_eq!(decide(&peer(0, 0, &[]), &p), Ok(()));
        assert_eq!(decide(&peer(1000, 1000, &[998, 960]), &p), Ok(()));
        assert_eq!(decide(&peer(1000, 960, &[]), &p), Ok(()), "the primary group counts too");
    }

    #[test]
    fn everyone_else_is_refused_with_what_to_do() {
        let p = Policy { daemon_uid: 0, mock: false, penna_gid: Some(960) };
        let e = decide(&peer(1000, 1000, &[998]), &p).unwrap_err();
        assert!(e.contains("usermod -aG penna"), "{e}");
        let none = Policy { penna_gid: None, ..p };
        assert!(decide(&peer(1000, 1000, &[]), &none).unwrap_err().contains("doesn't exist yet"));
    }

    #[test]
    fn the_mock_desk_lets_its_own_user_write_and_no_one_else() {
        let p = Policy { daemon_uid: 1000, mock: true, penna_gid: None };
        assert_eq!(decide(&peer(1000, 1000, &[]), &p), Ok(()));
        assert!(decide(&peer(1001, 1001, &[]), &p).is_err());
        let real = Policy { mock: false, ..p };
        assert!(decide(&peer(1000, 1000, &[]), &real).is_err(), "outside the mock, owning the daemon is not enough");
    }

    #[test]
    fn groups_come_from_etc_group_and_proc_status() {
        assert_eq!(group_gid("root:x:0:\nwheel:x:998:pmotta\npenna:x:960:pmotta\n", "penna"), Some(960));
        assert_eq!(group_gid("root:x:0:\n", "penna"), None);
        assert_eq!(parse_groups("Name:\tpenna\nGroups:\t3 90 998 1000 \nNSpid:\t1\n"), [3, 90, 998, 1000]);
        assert_eq!(parse_groups("Groups:\n"), Vec::<u32>::new());
    }
}
