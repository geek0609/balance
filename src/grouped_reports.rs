//! Group-level trial balance calculated from current ledger balances.
use crate::{AccountGroup, Balance, Error, Result, Store};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct GroupBalance {
    pub group: AccountGroup,
    /// Root-to-group names, inclusive.
    pub path: Vec<String>,
    pub direct_debit: i128,
    pub direct_credit: i128,
    /// Includes all descendant groups; parent and child rows must not be summed together.
    pub total_debit: i128,
    pub total_credit: i128,
}

#[derive(Debug, Clone)]
pub struct GroupTrialBalance {
    pub groups: Vec<GroupBalance>,
    pub ungrouped: Vec<Balance>,
    pub debit: i128,
    pub credit: i128,
}

impl Store {
    pub fn grouped_trial_balance(
        &self,
        company: i64,
        end: Option<&str>,
    ) -> Result<GroupTrialBalance> {
        if !self.companies()?.iter().any(|c| c.id == company) {
            return Err(Error::Validation("Company not found".into()));
        }
        let groups = self.account_groups(company)?;
        let index: HashMap<i64, &AccountGroup> =
            groups.iter().map(|group| (group.id, group)).collect();
        let mut paths: HashMap<i64, Vec<i64>> = HashMap::new();
        for group in &groups {
            let mut path = Vec::new();
            let mut visited = HashSet::new();
            let mut next = Some(group.id);
            while let Some(id) = next {
                if !visited.insert(id) {
                    return Err(Error::Validation(
                        "Account group hierarchy contains a cycle".into(),
                    ));
                }
                let ancestor = index.get(&id).ok_or_else(|| {
                    Error::Validation(
                        "Account group parent is missing or belongs to another company".into(),
                    )
                })?;
                if ancestor.category != group.category {
                    return Err(Error::Validation(
                        "Account group categories are inconsistent".into(),
                    ));
                }
                path.push(id);
                next = ancestor.parent_id;
            }
            paths.insert(group.id, path);
        }
        let mut amounts: HashMap<i64, (i128, i128, i128, i128)> = HashMap::new();
        let mut ungrouped = Vec::new();
        let (mut debit, mut credit) = (0_i128, 0_i128);
        for balance in self.balances_in_period(company, None, end)? {
            let signed = i128::from(balance.amount);
            let dr = signed.max(0);
            let cr = (-signed).max(0);
            debit += dr;
            credit += cr;
            if let Some(id) = balance.ledger.group_id {
                let group = index.get(&id).ok_or_else(|| {
                    Error::Validation(
                        "Ledger account group is missing or belongs to another company".into(),
                    )
                })?;
                if group.category != balance.ledger.category {
                    return Err(Error::Validation(
                        "Ledger and account group categories are inconsistent".into(),
                    ));
                }
                let direct = amounts.entry(id).or_default();
                direct.0 += dr;
                direct.1 += cr;
                for ancestor in &paths[&id] {
                    let total = amounts.entry(*ancestor).or_default();
                    total.2 += dr;
                    total.3 += cr;
                }
            } else {
                ungrouped.push(balance);
            }
        }
        let mut result = Vec::with_capacity(groups.len());
        for group in &groups {
            let path = paths[&group.id]
                .iter()
                .rev()
                .map(|id| index[id].name.clone())
                .collect();
            let (direct_debit, direct_credit, total_debit, total_credit) =
                amounts.remove(&group.id).unwrap_or_default();
            result.push(GroupBalance {
                group: group.clone(),
                path,
                direct_debit,
                direct_credit,
                total_debit,
                total_credit,
            });
        }
        result.sort_by(|a, b| {
            a.path
                .iter()
                .map(|s| s.to_lowercase())
                .cmp(b.path.iter().map(|s| s.to_lowercase()))
                .then_with(|| a.group.id.cmp(&b.group.id))
        });
        Ok(GroupTrialBalance {
            groups: result,
            ungrouped,
            debit,
            credit,
        })
    }
}
