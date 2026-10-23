//! Build editions, selected with Cargo features (`admin`, `headless`).

/// Which flavor of the client this binary is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edition {
    /// Window, tray, viewer and host.
    Standard,
    /// Standard plus the relay's device list.
    Admin,
    /// No window or tray: an unattended host only.
    Headless,
}

impl Edition {
    /// The edition this binary was compiled as.
    pub const fn current() -> Self {
        if cfg!(feature = "headless") {
            Edition::Headless
        } else if cfg!(feature = "admin") {
            Edition::Admin
        } else {
            Edition::Standard
        }
    }

    /// `platform` value sent at registration.
    pub fn platform_tag(self) -> &'static str {
        match self {
            Edition::Standard => "windows-pc",
            Edition::Admin => "windows-admin",
            Edition::Headless => "windows-host",
        }
    }

    /// Requests administrator rights from the relay (granted only by its credential list when configured).
    pub fn is_admin(self) -> bool {
        self == Edition::Admin
    }

    /// Headless hosts never view, so they do not advertise multi-session viewing.
    pub fn can_view(self) -> bool {
        self != Edition::Headless
    }

    /// Name used in logs.
    pub fn name(self) -> &'static str {
        match self {
            Edition::Standard => "standard",
            Edition::Admin => "admin",
            Edition::Headless => "headless-host",
        }
    }
}
