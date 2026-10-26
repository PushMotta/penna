//! Who may change fans (DESIGN.md §7.4). The socket is world-connectable; every connection's peer credentials decide once
//! whether its writes are allowed. Reads are open to any local user.

/// The connecting process, from `SO_PEERCRED` and `SO_PEERGROUPS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub uid: u32,
    pub gid: u32,
    /// Supplementary groups, as the process holds them: fixed when its session started.
    pub groups: Vec<u32>,
    /// Its user is listed in `penna`'s line of `/etc/group` now, so `usermod -aG penna` counts without logging in again.
    pub listed: bool,
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
        Some(g) if peer.gid == g || peer.groups.contains(&g) || peer.listed => Ok(()),
        Some(_) => Err("Changing fans needs membership of the penna group: sudo usermod -aG penna $USER (it applies at once).".into()),
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

/// Whether `uid`'s user name is a member of `group` in `/etc/group`, read now. Read per connection, so a `usermod -aG` made
/// while the daemon runs applies to the next connection.
pub fn listed(group: &str, uid: u32) -> bool {
    let (Ok(passwd), Ok(groups)) = (std::fs::read_to_string("/etc/passwd"), std::fs::read_to_string("/etc/group")) else {
        return false;
    };
    is_listed(&passwd, &groups, group, uid)
}

fn is_listed(etc_passwd: &str, etc_group: &str, group: &str, uid: u32) -> bool {
    let Some(name) = etc_passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        let name = f.next()?;
        (f.nth(1)?.parse::<u32>().ok()? == uid).then_some(name)
    }) else {
        return false;
    };
    etc_group.lines().any(|l| {
        let mut f = l.split(':');
        f.next() == Some(group) && f.nth(2).is_some_and(|members| members.split(',').any(|m| m.trim() == name))
    })
}

/// The peer's supplementary groups as the kernel recorded them when it connected (`SO_PEERGROUPS`, Linux 4.13). Not read from
/// `/proc/<pid>/status`: by the time that is read, the pid can belong to another process. None if the kernel can't say.
pub fn peer_groups(fd: std::os::fd::BorrowedFd<'_>) -> Vec<u32> {
    use std::os::fd::AsRawFd;
    let mut groups: Vec<libc::gid_t> = vec![0; 64];
    for _ in 0..2 {
        let mut len = (groups.len() * std::mem::size_of::<libc::gid_t>()) as libc::socklen_t;
        // SAFETY: `groups` has room for `len` bytes, and the kernel writes at most that many, or says how many it needs.
        let r = unsafe { libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERGROUPS, groups.as_mut_ptr().cast(), &mut len) };
        let n = len as usize / std::mem::size_of::<libc::gid_t>();
        if r == 0 {
            groups.truncate(n);
            return groups;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ERANGE) || n <= groups.len() {
            break;
        }
        groups.resize(n, 0);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uid: u32, gid: u32, groups: &[u32]) -> Peer {
        Peer { uid, gid, groups: groups.to_vec(), listed: false }
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
        assert!(e.contains("usermod -aG penna") && e.contains("applies at once"), "{e}");
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
    fn groups_come_from_etc_group_and_the_socket() {
        assert_eq!(group_gid("root:x:0:\nwheel:x:998:pmotta\npenna:x:960:pmotta\n", "penna"), Some(960));
        assert_eq!(group_gid("root:x:0:\n", "penna"), None);
        // The kernel's record of a connected peer is this process's own groups.
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut mine: Vec<u32> = std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("Groups:"))
            .map(|g| g.split_whitespace().filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default();
        let mut got = peer_groups(std::os::fd::AsFd::as_fd(&a));
        mine.sort();
        got.sort();
        assert_eq!(got, mine);
    }

    const PASSWD: &str =
        "root:x:0:0::/root:/usr/bin/bash\npmotta:x:1000:1000:Pedro Motta:/home/pmotta:/bin/bash\nguest:x:1001:1001::/home/guest:/bin/sh\n";

    #[test]
    fn a_user_added_to_penna_may_write_before_logging_in_again() {
        let group = "root:x:0:\nwheel:x:998:pmotta\npenna:x:960:pmotta\n";
        assert!(is_listed(PASSWD, group, "penna", 1000));
        assert!(!is_listed(PASSWD, group, "penna", 1001), "guest is not a member");
        assert!(!is_listed(PASSWD, group, "penna", 4242), "a uid with no user");
        assert!(is_listed(PASSWD, "penna:x:960:guest, pmotta\n", "penna", 1000), "members are a comma list");
        assert!(!is_listed(PASSWD, "pennant:x:961:pmotta\npenna:x:960:\n", "penna", 1000), "another group's line doesn't count");
        // The process's own groups still say nothing about penna: only the listing does.
        let p = Policy { daemon_uid: 0, mock: false, penna_gid: Some(960) };
        assert_eq!(decide(&Peer { listed: true, ..peer(1000, 1000, &[998]) }, &p), Ok(()));
    }
}
