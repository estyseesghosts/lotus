//! Fresh-root profile comparisons. A divergence is an observation, never a bug verdict.
use super::*;
use lotus_protocol::debug::MatrixProfile;

impl Supervisor {
    pub(super) fn browse_storage(
        &self,
        root: &Path,
        area: lotus_protocol::debug::StorageArea,
        path: &str,
        offset: u32,
        limit: u32,
    ) -> io::Result<serde_json::Value> {
        let base = root
            .join(match area {
                lotus_protocol::debug::StorageArea::Nand => "nand",
                lotus_protocol::debug::StorageArea::Sdmc => "sdmc",
            })
            .canonicalize()?;
        let relative = Path::new(path);
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(io::Error::other("storage path must stay below its area"));
        }
        let directory = base.join(relative).canonicalize()?;
        if !directory.starts_with(&base) {
            return Err(io::Error::other("storage path escaped session root"));
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(directory)? {
            if entries.len() == 10000 {
                return Err(io::Error::other(
                    "directory listing exceeds 10000-entry budget",
                ));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            entries.push(
                serde_json::json!({"name":entry.file_name().to_string_lossy(),
                "kind":if kind.is_symlink(){"link"}else if kind.is_dir(){"directory"}else{"file"},
                "bytes":if kind.is_file(){Some(entry.metadata()?.len())}else{None}}),
            );
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let total = entries.len();
        Ok(serde_json::json!({"area":area,"path":path,"total":total,
            "entries":entries.into_iter().skip(offset as usize).take(limit as usize).collect::<Vec<_>>(),
            "next":if (offset as usize)+(limit as usize)<total{Some(offset+limit)}else{None}}))
    }
    pub(super) async fn matrix(
        &self,
        id: &str,
        profiles: &[MatrixProfile],
        frames: u16,
        checkpoints: &[u16],
        repeats: u8,
        client: &str,
    ) -> io::Result<serde_json::Value> {
        let parent = self.handle(id)?;
        let asset = parent
            .info()
            .asset
            .ok_or_else(|| io::Error::other("matrix requires approved loaded content"))?;
        {
            let mut native = parent.native.lock().await;
            check_lease(&parent, client)?;
            parent.set_playing(false);
            pause_if_running(&mut native).await?;
        }
        // Resolve every selector before starting any target.
        for profile in profiles {
            if profile
                .backend
                .as_ref()
                .is_some_and(|name| !self.config.workers.contains_key(name))
            {
                return Err(io::Error::other(
                    "matrix backend is not a configured worker handle",
                ));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut rows = Vec::new();
        for profile in profiles {
            let worker = profile
                .backend
                .as_ref()
                .and_then(|name| self.config.workers.get(name))
                .map(PathBuf::as_path);
            let mut runs = Vec::new();
            for repeat in 0..repeats {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "matrix host deadline",
                    ));
                }
                let info = self
                    .create_seeded(
                        SessionKind::Secondary { parent: None },
                        None,
                        Actor::Host,
                        None,
                        worker,
                    )
                    .await?;
                let run=async {
                    let handle=self.handle(&info.id)?;
                    let mut native=handle.native.lock().await;
                    native.apply_profile(&profile.settings).await?;
                    native.load(&self.resolve_asset(&asset)?).await?;
                    let mut at=0;let mut snapshots=Vec::new();
                    for checkpoint in checkpoints {
                        let status=native.advance(checkpoint-at).await?; at = *checkpoint;
                        if status["frames"]!=*checkpoint { return Err(io::Error::other("matrix stopped before its aligned checkpoint")); }
                        let screens=native.capture().await?.into_iter().map(|frame|serde_json::json!({
                            "screen":frame.lease.screen,"width":frame.lease.width,"height":frame.lease.height,
                            "sha256":format!("{:x}",Sha256::digest(&frame.bgra))})).collect::<Vec<_>>();
                        let registers=native.registers(0).await?;
                        snapshots.push(serde_json::json!({"frame":checkpoint,"screens":screens,"registers":registers["registers"]}));
                        if Instant::now()>=deadline { return Err(io::Error::new(io::ErrorKind::TimedOut,"matrix host deadline")); }
                    }
                    if at<frames {native.advance(frames-at).await?;}
                    let status=native.status().await?;
                    Ok::<_,io::Error>(serde_json::json!({"repeat":repeat,"profile_hash":native.profile_hash(),
                        "identity":status["identity"],"renderer":status["identity"]["renderer"],
                        "checkpoints":snapshots,"final_status":status}))
                }.await;
                let stopped = self.stop(&info.id, Actor::Host).await;
                let value = run?;
                stopped?;
                runs.push(value);
            }
            let deterministic = runs
                .windows(2)
                .all(|pair| pair[0]["checkpoints"] == pair[1]["checkpoints"]);
            rows.push(serde_json::json!({"name":profile.name,"repeatable_at_checkpoints":if repeats>1 {Some(deterministic)}else{None},"runs":runs}));
        }
        let mut comparisons = Vec::new();
        for left in 0..rows.len() {
            for right in left + 1..rows.len() {
                let a = rows[left]["runs"][0]["checkpoints"]
                    .as_array()
                    .expect("owned snapshots");
                let b = rows[right]["runs"][0]["checkpoints"]
                    .as_array()
                    .expect("owned snapshots");
                let earliest = a.iter().zip(b).find(|(a, b)| {
                    a["screens"] != b["screens"] || a["registers"] != b["registers"]
                });
                let stable = rows[left]["repeatable_at_checkpoints"] == true
                    && rows[right]["repeatable_at_checkpoints"] == true;
                comparisons.push(serde_json::json!({"left":rows[left]["name"],"right":rows[right]["name"],
                "earliest_sampled_divergence":earliest.map(|(a,_)|a["frame"].clone()),
                "repeatability_confirmed":stable,"verdict":"observation; requires an independent reference to identify a bug"}));
            }
        }
        let build = self.builds.content_record(&asset);
        let report = serde_json::json!({"build":build,"profiles":rows,"comparisons":comparisons,
            "alignment":"presented guest frames; CPU state and screen hashes compared separately from host time",
            "source_session":id,"asset":asset,"evidence":"verified emulator run"});
        let artifact = self.publish_json(id, &report)?;
        Ok(serde_json::json!({"report":report,"artifacts":[artifact]}))
    }

    pub(super) fn publish_json(
        &self,
        session: &str,
        value: &serde_json::Value,
    ) -> io::Result<Artifact> {
        let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        if bytes.len() > 8 << 20 {
            return Err(io::Error::other("report artifact exceeds 8 MiB"));
        }
        let mut published = self.publish(
            session,
            vec![NewArtifact {
                bytes,
                screen: "report".into(),
                width: 0,
                height: 0,
                mime_type: "application/json".into(),
                extension: "json",
            }],
        )?;
        Ok(published.remove(0))
    }
}
