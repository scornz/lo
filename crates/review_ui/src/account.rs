//! Which account a pull request opens with (design §3 "Which account").

use std::fmt;

use github::accounts::{AccountChoice, RepoAccount};
use github::{GithubError, HelpLink};

/// Who a tab acts as on GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acting {
    /// A signed-in account, by login.
    Account(String),
    /// Nobody: a public repository, read without signing in.
    Anonymous,
}

impl Acting {
    pub fn login(&self) -> Option<&str> {
        match self {
            Self::Account(login) => Some(login),
            Self::Anonymous => None,
        }
    }
}

/// What to do about the account a pull request opens with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountDecision {
    Open(Acting),
    /// Several accounts can see the repository and none of them was chosen for it.
    Ask(Vec<String>),
    /// No account can open it, and GitHub said why.
    Refuse(Refusal),
}

/// Why rozed won't open a pull request with any account: the account remembered for its
/// repository, or for its owner, can't check the repository, or no account could and GitHub said
/// why. rozed doesn't fall back
/// to another account, which would review as someone else, nor open it without signing in,
/// which would hide what's wrong. GitHub's links say how to fix it, such as authorizing single
/// sign-on (design §3 "Blocked by an org", §11 "Single sign-on").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub message: String,
    pub links: Vec<HelpLink>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// Decides from what [`Accounts::account_for_repo`] found, or `None` when rozed has no accounts
/// database. A pull request opens with the account that can see its repository, and without
/// signing in when none can and none ran into anything else, which works for public
/// repositories. Messages name the repository as GitHub does now.
///
/// [`Accounts::account_for_repo`]: github::accounts::Accounts::account_for_repo
pub fn decide(found: Option<&RepoAccount>) -> AccountDecision {
    let Some(found) = found else {
        return AccountDecision::Open(Acting::Anonymous);
    };
    let (owner, repo) = (&found.owner, &found.name);
    match &found.choice {
        AccountChoice::Account(login) => AccountDecision::Open(Acting::Account(login.clone())),
        AccountChoice::Ask(logins) => AccountDecision::Ask(logins.clone()),
        AccountChoice::None => {
            // GitHub refusing a token signs its account out, which then counts as not signed
            // in, as it would had it been signed out before the check.
            let problems: Vec<_> = found
                .problems
                .iter()
                .filter(|(_, error)| !error.is_unauthorized())
                .collect();
            if problems.is_empty() {
                return AccountDecision::Open(Acting::Anonymous);
            }
            let said: Vec<String> = problems
                .iter()
                .map(|(login, error)| format!("{login}: {}", describe(error)))
                .collect();
            AccountDecision::Refuse(Refusal {
                message: format!(
                    "No account rozed is signed in to could check {owner}/{repo}, so it didn't \
                     open without signing in. {}",
                    said.join("; ")
                ),
                links: problems
                    .iter()
                    .filter_map(|(_, error)| error.help_link())
                    .collect(),
            })
        }
        AccountChoice::Unavailable(login) => {
            let problem = found
                .problems
                .iter()
                .find(|(problem, _)| problem.eq_ignore_ascii_case(login))
                .map(|(_, error)| error);
            let why = problem.map_or_else(|| "GitHub didn't say why".to_string(), describe);
            let for_repo = found
                .remembered
                .repo
                .as_deref()
                .is_some_and(|chosen| chosen.eq_ignore_ascii_case(login));
            let uses_for = if for_repo {
                format!("{owner}/{repo}")
            } else {
                owner.clone()
            };
            AccountDecision::Refuse(Refusal {
                message: format!(
                    "{login}, the account rozed uses for {uses_for}, couldn't check \
                     {owner}/{repo}: {why}"
                ),
                links: problem
                    .and_then(GithubError::help_link)
                    .into_iter()
                    .collect(),
            })
        }
    }
}

/// GitHub's message, and its link to fix it, where there's one.
fn describe(error: &GithubError) -> String {
    match error.help_link() {
        Some(link) => format!("{error} ({})", link.url()),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(choice: AccountChoice, problems: Vec<(&str, GithubError)>) -> RepoAccount {
        RepoAccount {
            owner: "org".into(),
            name: "repo".into(),
            choice,
            visible: Vec::new(),
            remembered: Default::default(),
            problems: problems
                .into_iter()
                .map(|(login, error)| (login.to_string(), error))
                .collect(),
        }
    }

    fn server_error() -> GithubError {
        GithubError::Api {
            status: 500,
            message: "Server Error".into(),
            documentation_url: None,
            errors: Vec::new(),
        }
    }

    #[test]
    fn without_an_accounts_database_it_opens_without_signing_in() {
        assert_eq!(decide(None), AccountDecision::Open(Acting::Anonymous));
    }

    #[test]
    fn the_account_that_can_see_the_repository_opens_it() {
        let found = found(AccountChoice::Account("rozed-bot".into()), vec![]);
        assert_eq!(
            decide(Some(&found)),
            AccountDecision::Open(Acting::Account("rozed-bot".into()))
        );
    }

    #[test]
    fn when_no_account_can_see_it_it_opens_without_signing_in() {
        let found = found(AccountChoice::None, vec![]);
        assert_eq!(
            decide(Some(&found)),
            AccountDecision::Open(Acting::Anonymous)
        );
    }

    #[test]
    fn several_accounts_with_none_chosen_for_the_owner_means_asking() {
        let found = found(AccountChoice::Ask(vec!["a".into(), "b".into()]), vec![]);
        assert_eq!(
            decide(Some(&found)),
            AccountDecision::Ask(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn a_remembered_account_that_couldnt_check_refuses_with_githubs_reason() {
        let found = found(
            AccountChoice::Unavailable("Work".into()),
            vec![("other", server_error()), ("work", server_error())],
        );
        assert_eq!(
            decide(Some(&found)),
            AccountDecision::Refuse(Refusal {
                message: "Work, the account rozed uses for org, couldn't check org/repo: GitHub \
                          answered 500: Server Error"
                    .into(),
                links: Vec::new(),
            })
        );
    }

    #[test]
    fn a_repositorys_own_account_that_couldnt_check_says_its_the_repositorys() {
        let mut found = found(
            AccountChoice::Unavailable("personal".into()),
            vec![("personal", server_error())],
        );
        found.remembered.repo = Some("personal".into());
        found.remembered.owner = Some("work".into());
        assert_eq!(
            refusal(decide(Some(&found))).message,
            "personal, the account rozed uses for org/repo, couldn't check org/repo: GitHub \
             answered 500: Server Error"
        );
    }

    #[test]
    fn messages_name_the_repository_as_github_does_now() {
        let mut found = found(
            AccountChoice::Unavailable("work".into()),
            vec![("work", server_error())],
        );
        found.owner = "bigorg".into();
        found.name = "tool".into();
        assert!(
            refusal(decide(Some(&found)))
                .message
                .starts_with("work, the account rozed uses for bigorg, couldn't check bigorg/tool")
        );
    }

    fn sso() -> GithubError {
        GithubError::SsoRequired {
            message: "Resource protected by organization SAML enforcement.".into(),
            url: Some("https://github.com/orgs/org/sso?authorization_request=abc".into()),
        }
    }

    fn oauth_restricted() -> GithubError {
        GithubError::OAuthAppRestricted {
            message: "Although you appear to have the correct authorization credentials, the \
                      `org` organization has enabled OAuth App access restrictions."
                .into(),
            org: Some("org".into()),
            documentation_url: None,
        }
    }

    fn refusal(decision: AccountDecision) -> Refusal {
        match decision {
            AccountDecision::Refuse(refusal) => refusal,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_remembered_account_that_needs_single_sign_on_refuses_with_githubs_link() {
        let found = found(
            AccountChoice::Unavailable("work".into()),
            vec![("work", sso())],
        );
        let refusal = refusal(decide(Some(&found)));
        assert_eq!(
            refusal.message,
            "work, the account rozed uses for org, couldn't check org/repo: Resource protected by \
             organization SAML enforcement. (https://github.com/orgs/org/sso?authorization_request=abc)"
        );
        let links: Vec<_> = refusal.links.iter().map(HelpLink::url).collect();
        assert_eq!(
            links,
            ["https://github.com/orgs/org/sso?authorization_request=abc"]
        );
    }

    #[test]
    fn a_remembered_account_an_organization_restricts_refuses_with_the_approval_link() {
        let found = found(
            AccountChoice::Unavailable("work".into()),
            vec![("work", oauth_restricted())],
        );
        let refusal = refusal(decide(Some(&found)));
        assert!(
            refusal
                .message
                .ends_with(&format!("({})", github::approval_url())),
            "{}",
            refusal.message
        );
        assert_eq!(
            refusal.links,
            oauth_restricted()
                .help_link()
                .into_iter()
                .collect::<Vec<_>>()
        );
        assert_eq!(refusal.links[0].label(), "Request approval on GitHub");
    }

    #[test]
    fn when_no_account_can_check_it_and_github_said_why_it_refuses_rather_than_open_anonymously() {
        let rate_limited = GithubError::RateLimited {
            message: "API rate limit exceeded".into(),
            secondary: false,
            reset: None,
            retry_after: None,
            resource: Some("core".into()),
        };
        let found = found(
            AccountChoice::None,
            vec![("home", rate_limited), ("work", sso())],
        );
        let refusal = refusal(decide(Some(&found)));
        assert_eq!(
            refusal.message,
            "No account rozed is signed in to could check org/repo, so it didn't open without \
             signing in. home: GitHub's rate limit was hit: API rate limit exceeded; work: \
             Resource protected by organization SAML enforcement. \
             (https://github.com/orgs/org/sso?authorization_request=abc)"
        );
        assert_eq!(
            refusal.links,
            sso().help_link().into_iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_token_github_refused_during_the_check_doesnt_stop_opening_without_signing_in() {
        // GitHub refusing a token signs its account out, which then counts as not signed in.
        let refused = GithubError::Unauthorized {
            message: "Bad credentials".into(),
        };
        let found = found(AccountChoice::None, vec![("old", refused)]);
        assert_eq!(
            decide(Some(&found)),
            AccountDecision::Open(Acting::Anonymous)
        );
    }
}
