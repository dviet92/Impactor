use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GsaAccount {
    email: String,
    first_name: String,
    adsid: String,
    xcode_gs_token: String,
    #[serde(default)]
    team_id: String,
    /// When this account was first added.
    ///
    /// Accounts stored before this was tracked have none. They all predate every
    /// account that does carry a date, so "unknown" sorts as oldest.
    #[serde(default)]
    added_at: Option<DateTime<Utc>>,
    /// Which shared device identity this account signed in with.
    ///
    /// `Some(n)` means the anisette state under `anisette/group-n/`; `None`
    /// means the machine-wide identity. Fixed at sign-in, because the tokens
    /// Apple issued are bound to the identity that asked for them: signing or
    /// refreshing with a different one gets them rejected. Changing the
    /// grouping setting therefore only affects later sign-ins.
    #[serde(default)]
    identity_group: Option<u32>,
    /// When this account last signed a device.
    ///
    /// Stamped at signing time, the same moment the device is recorded against
    /// the account, because that is when one of the account's three slots is
    /// spent. Unlike the device entries, which are dropped after seven days,
    /// this is kept for good: it is the only thing that tells an account that
    /// was never used from one whose week simply ran out.
    #[serde(default)]
    last_install_at: Option<DateTime<Utc>>,
}

impl GsaAccount {
    pub fn new(
        email: String,
        first_name: String,
        adsid: String,
        xcode_gs_token: String,
        team_id: String,
    ) -> Self {
        GsaAccount {
            email,
            first_name,
            adsid,
            xcode_gs_token,
            team_id,
            added_at: Some(Utc::now()),
            identity_group: None,
            last_install_at: None,
        }
    }
    pub fn email(&self) -> &String {
        &self.email
    }
    pub fn first_name(&self) -> &String {
        &self.first_name
    }
    pub fn adsid(&self) -> &String {
        &self.adsid
    }
    pub fn xcode_gs_token(&self) -> &String {
        &self.xcode_gs_token
    }
    pub fn team_id(&self) -> &String {
        &self.team_id
    }
    pub fn set_team_id(&mut self, team_id: String) {
        self.team_id = team_id;
    }
    pub fn added_at(&self) -> Option<DateTime<Utc>> {
        self.added_at
    }
    pub fn set_added_at(&mut self, added_at: Option<DateTime<Utc>>) {
        self.added_at = added_at;
    }
    pub fn identity_group(&self) -> Option<u32> {
        self.identity_group
    }
    pub fn set_identity_group(&mut self, group: Option<u32>) {
        self.identity_group = group;
    }
    pub fn last_install_at(&self) -> Option<DateTime<Utc>> {
        self.last_install_at
    }
    pub fn set_last_install_at(&mut self, at: Option<DateTime<Utc>>) {
        self.last_install_at = at;
    }
}

pub async fn account_from_session(
    email: String,
    account: plume_core::auth::Account,
) -> Result<GsaAccount, plume_core::Error> {
    let first_name = account.get_name().0;
    let s = plume_core::developer::DeveloperSession::using_account(account).await?;
    let teams_response = s.qh_list_teams().await?;
    let adsid = s.adsid().clone();
    let xcode_gs_token = s.xcode_gs_token().clone();

    let team_id = if teams_response.teams.is_empty() {
        "".to_string()
    } else {
        teams_response.teams[0].team_id.clone()
    };

    Ok(GsaAccount::new(
        email,
        first_name,
        adsid,
        xcode_gs_token,
        team_id,
    ))
}
