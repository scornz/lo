//! Which account a pull request opens with (design §3 "Which account").

use github::accounts::{AccountChoice, RepoAccount};

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
    /// Several accounts can see the repository and none was chosen for its owner yet.
    Ask(Vec<String>),
    /// The account remembered for the owner couldn't check the repository. rozed doesn't fall back
    /// to another account, which would review as someone else; the message says why.
    Refuse(String),
}

/// Decides from what [`Accounts::account_for_repo`] found, or `None` when rozed has no accounts
/// database. A pull request opens with the account that can see its repository, and without
/// signing in when none can, which works for public repositories.
///
/// [`Accounts::account_for_repo`]: github::accounts::Accounts::account_for_repo
pub fn decide(owner: &str, repo: &str, found: Option<&RepoAccount>) -> AccountDecision {
    let Some(found) = found else {
        return AccountDecision::Open(Acting::Anonymous);
    };
    match &found.choice {
        AccountChoice::Account(login) => AccountDecision::Open(Acting::Account(login.clone())),
        AccountChoice::Ask(logins) => AccountDecision::Ask(logins.clone()),
        AccountChoice::None => AccountDecision::Open(Acting::Anonymous),
        AccountChoice::Unavailable(login) => {
            let why = found
                .problems
                .iter()
                .find(|(problem, _)| problem.eq_ignore_ascii_case(login))
                .map_or_else(
                    || "GitHub didn't say why".to_string(),
                    |(_, error)| error.to_string(),
                );
            AccountDecision::Refuse(format!(
                "{login}, the account rozed uses for {owner}, couldn't check {owner}/{repo}: {why}"
            ))
        }
    }
}

/// What the accounts that couldn't check a repository ran into, one line each, to explain why a
/// pull request didn't open.
pub fn problems(found: Option<&RepoAccount>) -> Vec<String> {
    found
        .map(|found| {
            found
                .problems
                .iter()
                .map(|(login, error)| match error.help_link() {
                    Some(link) => format!("{login}: {error} ({})", link.url()),
                    None => format!("{login}: {error}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use github::GithubError;

    fn found(choice: AccountChoice, problems: Vec<(&str, GithubError)>) -> RepoAccount {
        RepoAccount {
            choice,
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
        assert_eq!(
            decide("sharkdp", "fd", None),
            AccountDecision::Open(Acting::Anonymous)
        );
    }

    #[test]
    fn the_account_that_can_see_the_repository_opens_it() {
        let found = found(AccountChoice::Account("rozed-bot".into()), vec![]);
        assert_eq!(
            decide("rozed-dev", "playground", Some(&found)),
            AccountDecision::Open(Acting::Account("rozed-bot".into()))
        );
    }

    #[test]
    fn when_no_account_can_see_it_it_opens_without_signing_in() {
        let found = found(AccountChoice::None, vec![]);
        assert_eq!(
            decide("sharkdp", "fd", Some(&found)),
            AccountDecision::Open(Acting::Anonymous)
        );
    }

    #[test]
    fn several_accounts_with_none_chosen_for_the_owner_means_asking() {
        let found = found(AccountChoice::Ask(vec!["a".into(), "b".into()]), vec![]);
        assert_eq!(
            decide("org", "repo", Some(&found)),
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
            decide("org", "repo", Some(&found)),
            AccountDecision::Refuse(
                "Work, the account rozed uses for org, couldn't check org/repo: GitHub answered \
                 500: Server Error"
                    .into()
            )
        );
    }

    #[test]
    fn problems_name_each_account_and_githubs_link() {
        let sso = GithubError::SsoRequired {
            message: "Resource protected by organization SAML enforcement.".into(),
            url: Some("https://github.com/orgs/org/sso?authorization_request=abc".into()),
        };
        let found = found(AccountChoice::None, vec![("work", sso)]);
        assert_eq!(
            problems(Some(&found)),
            vec![
                "work: Resource protected by organization SAML enforcement. \
                 (https://github.com/orgs/org/sso?authorization_request=abc)"
                    .to_string()
            ]
        );
        assert_eq!(problems(None), Vec::<String>::new());
    }
}
