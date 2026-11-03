//! The catalog published atomically by reload and config relax.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use crate::{PinnedFlags, Result};
use strata_gitio::Git;
use strata_session::catalog::CatalogSource;
use strata_store::DevPortsPolicy;
use strata_types::{Config, RepoName};

#[derive(Clone)]
pub struct Catalog {
    pub source: Config,
    pub config: Config,
    pub gits: BTreeMap<RepoName, Git>,
    pub pins: BTreeMap<RepoName, PinnedFlags>,
    pub file_flags: BTreeMap<RepoName, PinnedFlags>,
    pub pending_relax: Vec<RepoName>,
    pub dev_ports: Arc<DevPortsPolicy>,
    pub generation: u64,
    pub hash: String,
    pub loaded_at: SystemTime,
}

impl Catalog {
    pub fn build(
        source: Config,
        gits: BTreeMap<RepoName, Git>,
        pins: BTreeMap<RepoName, PinnedFlags>,
        generation: u64,
        hash: String,
    ) -> Result<Self> {
        let file_flags: BTreeMap<_, _> = source
            .repos
            .iter()
            .map(|(n, r)| (n.clone(), crate::confirm_flow::flags_of(r)))
            .collect();
        let pending_relax = file_flags
            .iter()
            .filter(|(n, f)| crate::confirm_flow::relaxes(&pins[*n], f))
            .map(|(n, _)| n.clone())
            .collect();
        let dev_ports = Arc::new(DevPortsPolicy::probing(source.dev_ports.blocks()?));
        let mut config = source.clone();
        for (n, r) in &mut config.repos {
            crate::confirm_flow::apply(r, &pins[n]);
        }
        Ok(Self {
            source,
            config,
            gits,
            pins,
            file_flags,
            pending_relax,
            dev_ports,
            generation,
            hash,
            loaded_at: SystemTime::now(),
        })
    }
}

impl CatalogSource for crate::startup::Core {
    fn config(&self) -> Result<&Config> {
        Ok(&self.snapshot()?.config)
    }
    fn gits(&self) -> Result<&BTreeMap<RepoName, Git>> {
        Ok(&self.snapshot()?.gits)
    }
    fn dev_ports(&self) -> Result<Option<&DevPortsPolicy>> {
        Ok(Some(&self.snapshot()?.dev_ports))
    }
}
