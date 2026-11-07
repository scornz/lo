//! `TrunkRules`: the branch rulesets that name a repository's trunks ([`crate::trunks`]). The open
//! pull request's poller keeps them in the cache, read again on the 5-minute refresh
//! ([`crate::poll::Poller::trunk_rules`]). A failure never fails the open pull request list: the
//! trunks are then its default branch and protected branches, and [`TrunkRules::unavailable`]
//! says why.

use std::sync::Arc;

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DateTime, not_found};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;
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

/// The rule types that gate merging into a branch: a ruleset holding one names trunks.
pub const GATING_RULES: [RepositoryRuleType; 5] = [
    RepositoryRuleType::PULL_REQUEST,
    RepositoryRuleType::MERGE_QUEUE,
    RepositoryRuleType::REQUIRED_STATUS_CHECKS,
    RepositoryRuleType::UPDATE,
    RepositoryRuleType::REQUIRED_DEPLOYMENTS,
];

/// The branch rulesets that name a repository's trunks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TrunkRuleList {
    /// The ref name conditions of the active rulesets with a rule in [`GATING_RULES`]. Those
    /// being evaluated or disabled, or that only enforce a policy, name no trunk.
    pub rulesets: Vec<RefNames>,
    /// Why the rulesets couldn't be read, the last time they were tried. `rulesets` then holds
    /// what was read before, if anything, and the read is tried again after a wait.
    pub unavailable: Option<String>,
}

impl TrunkRuleList {
    /// The rulesets that name trunks, from the query's answer.
    fn from_data(data: ResponseData) -> Option<Self> {
        let rulesets = data.repository?.rulesets?.nodes.unwrap_or_default();
        let gates = |ruleset: &TrunkRulesRepositoryRulesetsNodes| {
            (ruleset.rules.iter())
                .flat_map(|rules| rules.nodes.iter().flatten().flatten())
                .any(|rule| GATING_RULES.contains(&rule.type_))
        };
        let rulesets = (rulesets.into_iter().flatten())
            .filter(|ruleset| ruleset.enforcement == RuleEnforcement::ACTIVE && gates(ruleset))
            .filter_map(|ruleset| ruleset.conditions.ref_name)
            .map(|ref_name| RefNames {
                include: ref_name.include,
                exclude: ref_name.exclude,
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
            "{}\n{}\nrules naming trunks: {GATING_RULES:?}",
            schema_shape::<Self>(),
            QUERY
        )
    }
}

impl CachedRead for TrunkRuleList {
    /// Nothing about the open pull request: they're read again on the 5-minute refresh.
    const WATCHED: &'static [&'static str] = &[];

    fn is_complete(&self) -> bool {
        self.unavailable.is_none()
    }
}

impl GithubClient {
    /// The branch rulesets that name `repo`'s trunks. Any error but the network's, the rate
    /// limit's or the token's gives what `known` holds, or nothing, with the reason in
    /// [`TrunkRuleList::unavailable`].
    pub async fn trunk_rules(
        &self,
        repo: &RepoRef,
        known: Option<&TrunkRuleList>,
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
        match read {
            Err(error) if !stops_the_read(&error) => {
                log::warn!("reading {repo}'s rulesets: {error:#}");
                Ok(TrunkRuleList {
                    rulesets: known
                        .map(|known| known.rulesets.clone())
                        .unwrap_or_default(),
                    unavailable: Some(error.to_string()),
                })
            }
            read => read,
        }
    }
}

/// [`GithubClient::trunk_rules`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_trunk_rules(
    client: GithubClient,
    repo: RepoRef,
    known: Option<Arc<TrunkRuleList>>,
) -> Result<TrunkRuleList, GithubError> {
    client.trunk_rules(&repo, known.as_deref()).await
}
