//! [`ViewSet`] and [`ViewSets`]: an application's views, pages and library,
//! loaded once and stamped with a generation (TODO "Saltcorn UI" §4, TODO "The
//! builder" §8).
//!
//! The worker the views render on holds a snapshot of every view and page of an
//! application, because v1's `View.findOne` is synchronous. Sending that snapshot
//! on every render would be a megabyte of JSON per request, so it is sent when
//! its **generation** has moved — one integer comparison per render. This is
//! where the generation comes from: a read hands back the set that is cached, and
//! a write through [`ViewSets`] reloads the set and gives it a new generation.
//!
//! Generations come from one counter shared by every application, so a
//! generation is never reused and a reload always stamps a larger one. The stamp
//! is taken **before** the reload reads, and a set only replaces a cached one
//! with a smaller stamp: two writes racing cannot leave the cache holding the
//! older read.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use sc_app::AppId;
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use serde_json::Value as Json;

use crate::library::{
    self, LibraryItem, LibraryItemId, LibraryReferences, LibraryUpdate, collect_library_ids,
};
use crate::store;
use crate::view::{Page, View};

/// Every view, page and library item of one application, as of one generation.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewSet {
    /// The application.
    pub application: AppId,
    /// The generation this set was loaded at. A set with a different generation
    /// may hold different views; one with the same generation holds the same.
    pub generation: u64,
    /// The views, ordered by name.
    pub views: Vec<View>,
    /// The pages, ordered by name.
    pub pages: Vec<Page>,
    /// The library items, ordered by name.
    pub library: Vec<LibraryItem>,
}

impl ViewSet {
    /// Load every view and page of `application`, stamped with `generation`.
    pub async fn load(catalog: &Catalog, application: AppId, generation: u64) -> Result<ViewSet> {
        Ok(ViewSet {
            application,
            generation,
            views: store::list_views(catalog, application).await?,
            pages: store::list_pages(catalog, application).await?,
            library: library::list_library(catalog, application).await?,
        })
    }

    /// The view named `name`, if the set has one.
    pub fn view(&self, name: &str) -> Option<&View> {
        self.views.iter().find(|v| v.name == name)
    }

    /// The page named `name`, if the set has one.
    pub fn page(&self, name: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.name == name)
    }

    /// The library item `id`, if the set has one.
    pub fn library_item(&self, id: LibraryItemId) -> Option<&LibraryItem> {
        self.library.iter().find(|i| i.id == id)
    }

    /// The library items a view's configuration places, directly or inside an
    /// item it places, ordered by name.
    pub fn library_placed_by_view(&self, view: &View) -> Vec<&LibraryItem> {
        self.placed(view.configuration.values())
    }

    /// The library items a page's layout places, directly or inside an item it
    /// places, ordered by name.
    pub fn library_placed_by_page(&self, page: &Page) -> Vec<&LibraryItem> {
        self.placed(std::iter::once(&page.layout))
    }

    /// The library items an item's layout places, directly or nested, ordered by
    /// name. An item that places itself, however deeply, is in its own answer.
    pub fn library_placed_by_item(&self, item: &LibraryItem) -> Vec<&LibraryItem> {
        self.placed(std::iter::once(&item.layout))
    }

    /// The views, pages and other items that place the item `id`, directly or
    /// through an item they place.
    pub fn library_references(&self, id: LibraryItemId) -> LibraryReferences {
        let places = |items: Vec<&LibraryItem>| items.iter().any(|i| i.id == id);
        LibraryReferences {
            views: self
                .views
                .iter()
                .filter(|v| places(self.library_placed_by_view(v)))
                .map(|v| v.name.clone())
                .collect(),
            pages: self
                .pages
                .iter()
                .filter(|p| places(self.library_placed_by_page(p)))
                .map(|p| p.name.clone())
                .collect(),
            library: self
                .library
                .iter()
                .filter(|i| i.id != id && places(self.library_placed_by_item(i)))
                .map(|i| i.name.clone())
                .collect(),
        }
    }

    /// The set's items that `roots` place, followed through each item's own
    /// layout. An id that names no item of the set is skipped: it renders blank
    /// (v1's `resolveSegment`), so it places nothing. The visited set is what
    /// stops an item that contains itself.
    fn placed<'a>(&self, roots: impl Iterator<Item = &'a Json>) -> Vec<&LibraryItem> {
        let mut pending = BTreeSet::new();
        for root in roots {
            collect_library_ids(root, &mut pending);
        }
        let mut seen: BTreeSet<LibraryItemId> = BTreeSet::new();
        while let Some(raw) = pending.pop_first() {
            let Some(item) = uuid::Uuid::parse_str(&raw)
                .ok()
                .and_then(|id| self.library_item(LibraryItemId(id)))
            else {
                continue;
            };
            if seen.insert(item.id) {
                collect_library_ids(&item.layout, &mut pending);
            }
        }
        // `library` is ordered by name, so filtering it keeps that order.
        self.library
            .iter()
            .filter(|i| seen.contains(&i.id))
            .collect()
    }
}

/// The cache of [`ViewSet`]s, one per application, and the writes that keep it
/// current.
#[derive(Debug)]
pub struct ViewSets {
    sets: RwLock<HashMap<AppId, Arc<ViewSet>>>,
    next_generation: AtomicU64,
}

impl Default for ViewSets {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewSets {
    /// An empty cache.
    pub fn new() -> ViewSets {
        ViewSets {
            sets: RwLock::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
        }
    }

    /// The set for `application`: the cached one, or loaded now and cached. A
    /// read never moves the generation of a set that is already cached.
    pub async fn get(&self, catalog: &Catalog, application: AppId) -> Result<Arc<ViewSet>> {
        if let Some(set) = self.cached(application)? {
            return Ok(set);
        }
        self.load(catalog, application).await
    }

    /// Reload the set for `application` under a new generation — what a view or
    /// page write, an application save and `SIGHUP` do.
    pub async fn reload(&self, catalog: &Catalog, application: AppId) -> Result<Arc<ViewSet>> {
        self.load(catalog, application).await
    }

    /// The generation of the cached set for `application`, if one is cached.
    pub fn generation(&self, application: AppId) -> Result<Option<u64>> {
        Ok(self.cached(application)?.map(|s| s.generation))
    }

    /// Drop the cached set for `application` — what deleting the application
    /// does.
    pub fn forget(&self, application: AppId) -> Result<()> {
        self.sets
            .write()
            .map_err(|_| poisoned())?
            .remove(&application);
        Ok(())
    }

    /// [`store::save_view`], then reload the view's application.
    pub async fn save_view(&self, catalog: &Catalog, view: &View) -> Result<View> {
        let saved = store::save_view(catalog, view).await?;
        self.reload(catalog, saved.application).await?;
        Ok(saved)
    }

    /// [`store::delete_view`], then reload the application.
    pub async fn delete_view(
        &self,
        catalog: &Catalog,
        application: AppId,
        name: &str,
    ) -> Result<bool> {
        let deleted = store::delete_view(catalog, application, name).await?;
        self.reload(catalog, application).await?;
        Ok(deleted)
    }

    /// [`store::save_page`], then reload the page's application.
    pub async fn save_page(&self, catalog: &Catalog, page: &Page) -> Result<Page> {
        let saved = store::save_page(catalog, page).await?;
        self.reload(catalog, saved.application).await?;
        Ok(saved)
    }

    /// [`store::delete_page`], then reload the application.
    pub async fn delete_page(
        &self,
        catalog: &Catalog,
        application: AppId,
        name: &str,
    ) -> Result<bool> {
        let deleted = store::delete_page(catalog, application, name).await?;
        self.reload(catalog, application).await?;
        Ok(deleted)
    }

    /// [`library::save_library_item`], then reload the item's application.
    pub async fn save_library_item(
        &self,
        catalog: &Catalog,
        item: &LibraryItem,
    ) -> Result<LibraryItem> {
        let saved = library::save_library_item(catalog, item).await?;
        self.reload(catalog, saved.application).await?;
        Ok(saved)
    }

    /// [`library::delete_library_item`], then reload the application.
    pub async fn delete_library_item(
        &self,
        catalog: &Catalog,
        application: AppId,
        id: LibraryItemId,
    ) -> Result<bool> {
        let deleted = library::delete_library_item(catalog, application, id).await?;
        self.reload(catalog, application).await?;
        Ok(deleted)
    }

    /// [`library::apply_library_updates`], then reload the application.
    pub async fn apply_library_updates(
        &self,
        catalog: &Catalog,
        application: AppId,
        updates: &[LibraryUpdate],
    ) -> Result<()> {
        library::apply_library_updates(catalog, application, updates).await?;
        self.reload(catalog, application).await?;
        Ok(())
    }

    fn cached(&self, application: AppId) -> Result<Option<Arc<ViewSet>>> {
        Ok(self
            .sets
            .read()
            .map_err(|_| poisoned())?
            .get(&application)
            .cloned())
    }

    /// Load under a fresh stamp and cache the result unless a newer stamp got
    /// there first, in which case the newer set is the answer.
    async fn load(&self, catalog: &Catalog, application: AppId) -> Result<Arc<ViewSet>> {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let loaded = Arc::new(ViewSet::load(catalog, application, generation).await?);
        let mut sets = self.sets.write().map_err(|_| poisoned())?;
        match sets.get(&application) {
            Some(newer) if newer.generation > generation => Ok(newer.clone()),
            _ => {
                sets.insert(application, loaded.clone());
                Ok(loaded)
            }
        }
    }
}

fn poisoned() -> Error {
    Error::msg("the view set cache lock is poisoned")
}
