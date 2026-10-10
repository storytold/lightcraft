//! The publish-service boundary.
//!
//! Built-in publish services (the `publish` crate) and plug-in services implement the same shape.
//! [`PublishProvider`] is that shape as seen from here; the publish crate wires a
//! [`PluginPublisher`] into its own service trait with a thin adapter, so this crate doesn't depend
//! on it (and stays below it in the layer stack).
//!
//! Requests the plug-in receives:
//! - `{"hook": "publish.upload", "service", "item": {path, photo, title, caption, keywords, remoteId?}}`
//!   → `{remoteId, url?}`; `item.path` (the rendered file) is lent to the call for reading.
//! - `{"hook": "publish.delete", "service", "remoteId"}` → anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::HostApi;
use crate::manager::Manager;
use crate::{Error, Result};

/// One rendered photo to publish.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishItem {
    /// The rendered file.
    pub path: String,
    /// Catalog photo id.
    pub photo: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Set when re-publishing a photo the service already has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
}

/// Where the service put it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedRef {
    pub remote_id: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// A publish service.
pub trait PublishProvider {
    fn service_id(&self) -> &str;
    fn label(&self) -> &str;
    fn upload(&mut self, host: &mut dyn HostApi, item: &PublishItem) -> Result<PublishedRef>;
    fn delete(&mut self, host: &mut dyn HostApi, remote_id: &str) -> Result<()>;
}

/// A plug-in's publish service.
pub struct PluginPublisher<'m> {
    manager: &'m mut Manager,
    plugin: String,
    service: crate::PublishDecl,
}

impl<'m> PluginPublisher<'m> {
    /// The service of plug-in `plugin`, if it is installed, enabled and provides one.
    pub fn new(manager: &'m mut Manager, plugin: &str) -> Result<PluginPublisher<'m>> {
        let p = manager.get(plugin).ok_or_else(|| Error::NotFound(plugin.into()))?;
        if !p.enabled {
            return Err(Error::Disabled(plugin.into()));
        }
        let service = p.plugin.manifest().publish.clone().ok_or_else(|| Error::Params(format!("plug-in {plugin} provides no publish service")))?;
        Ok(PluginPublisher { manager, plugin: plugin.into(), service })
    }
}

impl PublishProvider for PluginPublisher<'_> {
    fn service_id(&self) -> &str {
        &self.service.id
    }
    fn label(&self) -> &str {
        &self.service.name
    }
    fn upload(&mut self, host: &mut dyn HostApi, item: &PublishItem) -> Result<PublishedRef> {
        let req = json!({"hook": "publish.upload", "service": self.service.id, "item": item});
        let v = self.manager.invoke(&self.plugin, &req, host, &[PathBuf::from(&item.path)])?;
        serde_json::from_value(v).map_err(|e| Error::Failed(format!("publish.upload reply: {e}")))
    }
    fn delete(&mut self, host: &mut dyn HostApi, remote_id: &str) -> Result<()> {
        let req = json!({"hook": "publish.delete", "service": self.service.id, "remoteId": remote_id});
        self.manager.invoke(&self.plugin, &req, host, &[]).map(|_: Value| ())
    }
}
