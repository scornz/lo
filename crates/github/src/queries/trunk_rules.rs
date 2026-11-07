//! `TrunkRules`: the branch rulesets that name a repository's trunks ([`crate::trunks`]). The
//! repository's poller reads them with its open pull requests, and keeps them in the cache
//! ([`crate::poll::Poller::trunk_rules`]). A failure never fails the open pull request list: the
//! rulesets read before stay, the branches open pull requests both target and come from are
//! checked one by one instead, and [`TrunkRuleList::unavailable`] says why.

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DateTime, not_found};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::pulls::RepoRef;
use crate::push_history::stops_the_read;
use crate::trunks::RefNames;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/trunk_rules.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct TrunkRules;

// The generated types, named from this module, such as `queries::trunk_rules::Variables`.
pub use self::trunk_rules::*;

/// The rule types that gate merging into a branch: a ruleset holding one names the branches its
/// patterns match as trunks.
pub const GATING_RULES: [RepositoryRuleType; 5] = [
    RepositoryRuleType::PULL_REQUEST,
    RepositoryRuleType::MERGE_QUEUE,
    RepositoryRuleType::REQUIRED_STATUS_CHECKS,
    RepositoryRuleType::UPDATE,
    RepositoryRuleType::REQUIRED_DEPLOYMENTS,
];

/// [`GATING_RULES`] as the REST API names them.
const GATING_REST_RULES: [&str; 5] = [
    "pull_request",
    "merge_queue",
    "required_status_checks",
    "update",
    "required_deployments",
];

/// The most branches checked one by one when the rulesets can't be read.
pub const MAX_BRANCHES_CHECKED: usize = 10;

/// The branch rulesets that name a repository's trunks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TrunkRuleList {
    /// The ref name conditions of the active rulesets, each saying whether it gates merging.
    /// Rulesets being evaluated or disabled name no trunk.
    pub rulesets: Vec<RefNames>,
    /// Why the rulesets couldn't be read, the last time they were tried. `rulesets` then holds
    /// what was read before, if anything, and an exact include for each branch checked one by one
    /// that a gating rule covers.
    pub unavailable: Option<String>,
}

impl TrunkRuleList {
    /// The active rulesets, from the query's answer.
    fn from_data(data: ResponseData) -> Option<Self> {
        let rulesets = data.repository?.rulesets?.nodes.unwrap_or_default();
        let gates = |ruleset: &TrunkRulesRepositoryRulesetsNodes| {
            (ruleset.rules.iter())
                .flat_map(|rules| rules.nodes.iter().flatten().flatten())
                .any(|rule| GATING_RULES.contains(&rule.type_))
        };
        let rulesets = (rulesets.into_iter().flatten())
            .filter(|ruleset| ruleset.enforcement == RuleEnforcement::ACTIVE)
            .filter_map(|ruleset| {
                let gates_merging = gates(&ruleset);
                let ref_name = ruleset.conditions.ref_name?;
                Some(RefNames {
                    include: ref_name.include,
                    exclude: ref_name.exclude,
                    gates_merging,
                })
            })
            .collect();
        Some(Self {
            rulesets,
            unavailable: None,
        })
    }
}

impl CacheEntry for TrunkRuleList {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!(
            "{}\n{}\nrules gating merges: {GATING_RULES:?}",
            schema_shape::<Self>(),
            QUERY
        )
    }
}

/// One of the rules that apply to a branch, as `GET /repos/{owner}/{repo}/rules/branches/{branch}`
/// lists them.
#[derive(Deserialize)]
struct BranchRule {
    #[serde(rename = "type")]
    type_: String,
}

impl GithubClient {
    /// The branch rulesets that name `repo`'s trunks. Any error but the network's, the rate
    /// limit's or the token's gives what `known` holds, or nothing, with the reason in
    /// [`TrunkRuleList::unavailable`]. Then each of `branches`, at most
    /// [`MAX_BRANCHES_CHECKED`], is checked one by one through REST, which anyone who can read
    /// the repository may ask.
    pub async fn trunk_rules(
        &self,
        repo: &RepoRef,
        known: Option<&TrunkRuleList>,
        branches: &[String],
    ) -> Result<TrunkRuleList, GithubError> {
        let variables = Variables {
            owner: repo.owner.clone(),
            name: repo.name.clone(),
        };
        let read = (self
            .graphql_keeping_partial::<TrunkRules>(variables, 2)
            .await)
            .and_then(|data| {
                TrunkRuleList::from_data(data)
                    .ok_or_else(|| not_found(format!("rulesets of {repo}")))
            });
        let error = match read {
            Err(error) if !stops_the_read(&error) => error,
            read => return read,
        };
        log::warn!("reading {repo}'s rulesets: {error:#}");
        let mut rules = TrunkRuleList {
            rulesets: known
                .map(|known| known.rulesets.clone())
                .unwrap_or_default(),
            unavailable: Some(error.to_string()),
        };
        for branch in branches.iter().take(MAX_BRANCHES_CHECKED) {
            let include = format!("refs/heads/{branch}");
            let named = |ruleset: &RefNames| ruleset.include == [include.as_str()];
            if rules.rulesets.iter().any(named) {
                continue;
            }
            let path = format!(
                "/repos/{}/{}/rules/branches/{}",
                repo.owner,
                repo.name,
                path_encoded(branch)
            );
            match self.get::<Vec<BranchRule>>(&path).await {
                Ok(applied) => {
                    let gates =
                        |rule: &BranchRule| GATING_REST_RULES.contains(&rule.type_.as_str());
                    if applied.iter().any(gates) {
                        rules.rulesets.push(RefNames {
                            include: vec![include],
                            exclude: Vec::new(),
                            gates_merging: true,
                        });
                    }
                }
                Err(error) if stops_the_read(&error) => return Err(error),
                Err(error) => {
                    log::warn!("reading the rules on {repo} {branch}: {error:#}");
                    break;
                }
            }
        }
        Ok(rules)
    }
}

/// A branch's name for a URL's path: its slashes kept, anything else but letters, digits and
/// `-._~` percent-encoded, such as the `#` a branch's name may hold.
fn path_encoded(branch: &str) -> String {
    let mut encoded = String::with_capacity(branch.len());
    for byte in branch.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_are_encoded_for_the_path() {
        assert_eq!(path_encoded("release/2.0"), "release/2.0");
        assert_eq!(path_encoded("fix#12 é"), "fix%2312%20%C3%A9");
    }
}
