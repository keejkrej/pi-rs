//! Port of packages/chord/src/facets/host.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use indexmap::{IndexMap, IndexSet};

use crate::context::BACKGROUND_CONTEXT;
use crate::services::consumer::RemoteServiceBindingImpl;
use crate::services::errors::AggregateError;
use crate::services::handle::{ServiceObject, ServiceSlot};
use crate::services::instances::{InstanceDirectory, InstanceDirectoryEntry, InstanceDirectoryOptions};
use crate::services::loopback::create_loopback_service_transport;
use crate::services::provider::{
    RemoteServiceProvider, RemoteServiceProviderEntry, validate_remote_service_implementation,
};
use crate::types::{
    Context, Facet, FacetEnvironment, FacetEnvironmentHandlers, FacetOptions, RemoteServiceBinding,
    RemoteServiceBindingOptions, RemoteServiceSource, RemoteServiceSourceOpenOptions, RemoteServices, Service,
    ServiceIdRef, ServiceMode, ServiceSpawnerFn,
};

type ErrorReporter = Arc<dyn Fn(pi_js::Error) + Send + Sync>;
type AssertAccess = Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>;
type Disposal = Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ActivateCallback = Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ObserveStart = Arc<dyn Fn() -> pi_js::Result<Disposal> + Send + Sync>;
type ObserveHandler = Arc<dyn Fn(ServiceObject, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type FallibleRelease = Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>;
type ServiceInstanceInstaller = Arc<dyn Fn(&str, ServiceObject) -> pi_js::Result<FallibleRelease> + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LifecycleState {
    SettingUp,
    Prepared,
    Active,
    Disposing,
    Dead,
}

impl LifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::SettingUp => "setting_up",
            Self::Prepared => "prepared",
            Self::Active => "active",
            Self::Disposing => "disposing",
            Self::Dead => "dead",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GenerationPhase {
    Setup,
    Assembling,
    Connecting,
    Activating,
    Active,
    Reloading,
    Disposing,
    Dead,
}

impl GenerationPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Assembling => "assembling",
            Self::Connecting => "connecting",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Reloading => "reloading",
            Self::Disposing => "disposing",
            Self::Dead => "dead",
        }
    }
}

#[derive(Clone)]
struct FacetServiceReference {
    service_id: String,
    service: Service,
    mode: ServiceMode,
}

#[derive(Clone)]
struct ExternalService {
    service: Service,
    mode: ServiceMode,
    source: RemoteServiceSource,
}

#[derive(Clone)]
struct FacetShape {
    facet_id: String,
    requires: Vec<FacetServiceReference>,
    provides: Vec<FacetServiceReference>,
}

#[derive(Clone)]
struct FacetLifecycle {
    inner: Arc<FacetLifecycleInner>,
}

struct FacetLifecycleInner {
    id: String,
    effects: Mutex<Vec<Disposal>>,
    observations: Mutex<Vec<ObserveStart>>,
    activate: Mutex<Vec<ActivateCallback>>,
    state: Mutex<LifecycleState>,
    service_access: AtomicBool,
}

impl FacetLifecycle {
    fn new(id: &str) -> Self {
        Self {
            inner: Arc::new(FacetLifecycleInner {
                id: id.to_string(),
                effects: Mutex::new(Vec::new()),
                observations: Mutex::new(Vec::new()),
                activate: Mutex::new(Vec::new()),
                state: Mutex::new(LifecycleState::SettingUp),
                service_access: AtomicBool::new(false),
            }),
        }
    }

    fn assert_setting_up(&self, operation: &str) -> pi_js::Result<()> {
        let state = *self.inner.state.lock().unwrap();
        if state != LifecycleState::SettingUp {
            return Err(pi_js::Error::js(
                "Error",
                format!("Facet {} can {operation} only during setup", self.inner.id),
            ));
        }
        Ok(())
    }

    fn assert_running(&self, operation: &str) -> pi_js::Result<()> {
        let state = *self.inner.state.lock().unwrap();
        if state != LifecycleState::SettingUp && state != LifecycleState::Active {
            return Err(pi_js::Error::js(
                "Error",
                format!("Facet {} cannot {operation} while {}", self.inner.id, state.as_str()),
            ));
        }
        Ok(())
    }

    fn assert_active(&self, operation: &str) -> pi_js::Result<()> {
        let state = *self.inner.state.lock().unwrap();
        if state != LifecycleState::Active {
            return Err(pi_js::Error::js(
                "Error",
                format!("Facet {} can {operation} only while active", self.inner.id),
            ));
        }
        Ok(())
    }

    fn assert_service_access(&self) -> pi_js::Result<()> {
        if !self.inner.service_access.load(Ordering::Acquire) {
            let state = *self.inner.state.lock().unwrap();
            return Err(pi_js::Error::js(
                "Error",
                format!(
                    "Facet {} service handles cannot be used while {}",
                    self.inner.id,
                    state.as_str()
                ),
            ));
        }
        Ok(())
    }

    fn revoke(&self) {
        self.inner.service_access.store(false, Ordering::Release);
    }

    fn own(&self, disposal: Disposal) -> pi_js::Result<()> {
        self.assert_running("own resources")?;
        self.inner.effects.lock().unwrap().push(disposal);
        Ok(())
    }

    fn observe(&self, start: ObserveStart) -> pi_js::Result<()> {
        self.assert_setting_up("observe services")?;
        self.inner.observations.lock().unwrap().push(start);
        Ok(())
    }

    fn on_activate(&self, callback: ActivateCallback) -> pi_js::Result<()> {
        self.assert_setting_up("register activation callbacks")?;
        self.inner.activate.lock().unwrap().push(callback);
        Ok(())
    }

    fn prepared(&self) -> pi_js::Result<()> {
        self.assert_setting_up("finish setup")?;
        *self.inner.state.lock().unwrap() = LifecycleState::Prepared;
        Ok(())
    }

    async fn activate(&self) -> pi_js::Result<()> {
        let (observations, callbacks) = {
            let mut state = self.inner.state.lock().unwrap();
            if *state != LifecycleState::Prepared {
                return Err(pi_js::Error::js(
                    "Error",
                    format!("Facet {} is not prepared", self.inner.id),
                ));
            }
            *state = LifecycleState::Active;
            self.inner.service_access.store(true, Ordering::Release);
            let observations = std::mem::take(&mut *self.inner.observations.lock().unwrap());
            let callbacks = std::mem::take(&mut *self.inner.activate.lock().unwrap());
            (observations, callbacks)
        };

        for callback in callbacks {
            callback().await?;
        }
        for start in observations {
            let disposal = start()?;
            self.inner.effects.lock().unwrap().push(disposal);
        }
        Ok(())
    }

    async fn dispose(&self) -> pi_js::Result<()> {
        let effects = {
            let mut state = self.inner.state.lock().unwrap();
            if *state == LifecycleState::Dead {
                return Ok(());
            }
            *state = LifecycleState::Disposing;
            self.inner.observations.lock().unwrap().clear();
            self.inner.activate.lock().unwrap().clear();
            std::mem::take(&mut *self.inner.effects.lock().unwrap())
        };

        let mut errors = Vec::new();
        for effect in effects.into_iter().rev() {
            if let Err(error) = effect().await {
                errors.push(error);
            }
        }

        self.inner.service_access.store(false, Ordering::Release);
        *self.inner.state.lock().unwrap() = LifecycleState::Dead;

        crate::services::errors::throw_collected_errors(&errors, &format!("Failed to dispose facet {}", self.inner.id))
    }
}

#[derive(Clone)]
struct LocalKeyedRegistration {
    generations: Arc<Mutex<IndexMap<String, i64>>>,
    directory: InstanceDirectory<InstanceDirectoryEntry>,
}

#[derive(Clone)]
struct LocalKeyedServiceRegistry {
    inner: Arc<LocalKeyedServiceRegistryInner>,
}

struct LocalKeyedServiceRegistryInner {
    registrations: Mutex<IndexMap<String, LocalKeyedRegistration>>,
    disposed: AtomicBool,
}

impl LocalKeyedServiceRegistry {
    fn new(services: &[Service], on_error: ErrorReporter) -> pi_js::Result<Self> {
        let mut seen = IndexSet::new();
        let mut registrations = IndexMap::new();
        for service in services {
            if !seen.insert(service.id.clone()) {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Local keyed service registry has duplicate IDs",
                ));
            }
            registrations.insert(
                service.id.clone(),
                LocalKeyedRegistration {
                    generations: Arc::new(Mutex::new(IndexMap::new())),
                    directory: InstanceDirectory::new(InstanceDirectoryOptions {
                        ready: true,
                        on_error: Arc::clone(&on_error),
                    }),
                },
            );
        }
        Ok(Self {
            inner: Arc::new(LocalKeyedServiceRegistryInner {
                registrations: Mutex::new(registrations),
                disposed: AtomicBool::new(false),
            }),
        })
    }

    fn spawn(&self, service: &Service, key: &str, implementation: ServiceObject) -> pi_js::Result<pi_js::Unsubscribe> {
        self.assert_active()?;
        if key.is_empty() {
            return Err(pi_js::Error::js(
                "TypeError",
                "Local service instance key must not be empty",
            ));
        }
        if !is_plain_object(&implementation) {
            return Err(pi_js::Error::js(
                "TypeError",
                format!("Local service {} implementation must be an object", service.id),
            ));
        }

        let registration = self.registration(&service.id)?;
        if registration.directory.get(key).is_some() {
            return Err(pi_js::Error::js(
                "Error",
                format!(
                    "Local service {} already has a live instance with key {key}",
                    service.id
                ),
            ));
        }

        let generation = {
            let mut generations = registration.generations.lock().unwrap();
            let next = generations.get(key).copied().unwrap_or(0) + 1;
            generations.insert(key.to_string(), next);
            next
        };

        let instance = InstanceDirectoryEntry::new(key, generation, implementation, Arc::new(|| {}));
        registration.directory.insert(instance.clone())?;

        let closed = AtomicBool::new(false);
        let directory = registration.directory.clone();
        Ok(pi_js::Unsubscribe::new(move || {
            if closed.swap(true, Ordering::AcqRel) {
                return;
            }
            directory.remove(&instance);
        }))
    }

    fn observe(&self, service: &Service, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        self.assert_active()?;
        let registration = self.registration(&service.id)?;
        registration.directory.observe(handler)
    }

    fn dispose(&self) {
        if self.inner.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        let registrations: Vec<LocalKeyedRegistration> = {
            let mut guard = self.inner.registrations.lock().unwrap();
            let drained = std::mem::take(&mut *guard);
            drained.into_values().collect()
        };
        for registration in registrations {
            registration.directory.dispose();
        }
    }

    fn registration(&self, service_id: &str) -> pi_js::Result<LocalKeyedRegistration> {
        self.inner
            .registrations
            .lock()
            .unwrap()
            .get(service_id)
            .cloned()
            .ok_or_else(|| pi_js::Error::js("Error", format!("Local keyed service {service_id} is not registered")))
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        if self.inner.disposed.load(Ordering::Acquire) {
            return Err(pi_js::Error::js("Error", "Local keyed service registry is disposed"));
        }
        Ok(())
    }
}

// PORT: structural `KeyedServiceSource`. Remote services use `RemoteServices`, local ones use `LocalKeyedServiceRegistry`.
#[derive(Clone)]
enum KeyedServiceSource {
    Local(LocalKeyedServiceRegistry),
    Remote(RemoteServices),
}

impl KeyedServiceSource {
    fn observe(&self, service: &Service, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        match self {
            Self::Local(registry) => registry.observe(service, handler),
            Self::Remote(remote) => remote.observe::<ServiceObject>(service, handler),
        }
    }
}

#[derive(Clone)]
struct HostServiceSlots {
    inner: Arc<HostServiceSlotsInner>,
}

struct HostServiceSlotsInner {
    singletons: Mutex<IndexMap<String, ServiceSlot>>,
    keyed_sources: Mutex<IndexMap<String, KeyedServiceSource>>,
}

impl HostServiceSlots {
    fn new() -> Self {
        Self {
            inner: Arc::new(HostServiceSlotsInner {
                singletons: Mutex::new(IndexMap::new()),
                keyed_sources: Mutex::new(IndexMap::new()),
            }),
        }
    }

    fn get_singleton(&self, service: &Service, assert_access: AssertAccess) -> ServiceObject {
        let slot = {
            let mut singletons = self.inner.singletons.lock().unwrap();
            singletons
                .entry(service.id.clone())
                .or_insert_with(|| ServiceSlot::new(&service.id, !service.local))
                .clone()
        };
        slot.view(assert_access)
    }

    fn has_singleton(&self, service_id: &str) -> bool {
        self.inner.singletons.lock().unwrap().contains_key(service_id)
    }

    fn observe(
        &self,
        service: &Service,
        assert_access: AssertAccess,
        handler: ObserveHandler,
    ) -> pi_js::Result<Disposal> {
        let source = self
            .inner
            .keyed_sources
            .lock()
            .unwrap()
            .get(&service.id)
            .cloned()
            .ok_or_else(|| pi_js::Error::js("Error", format!("Service {} is disconnected", service.id)))?;

        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_for_view = Arc::clone(&stopped);
        let service_id = service.id.clone();
        let wrap_objects = !service.local;
        let stop = Mutex::new(Some(source.observe(
            service,
            Arc::new(move |target, context| {
                let slot = ServiceSlot::new(&service_id, wrap_objects);
                slot.bind(target);
                let assert_access = Arc::clone(&assert_access);
                let stopped_view = Arc::clone(&stopped_for_view);
                let ctx = context.clone();
                let sid = service_id.clone();
                let view = slot.view(Arc::new(move || {
                    assert_access()?;
                    if stopped_view.load(Ordering::Acquire) || ctx.abort_signal().is_some_and(|signal| signal.aborted())
                    {
                        return Err(pi_js::Error::js(
                            "Error",
                            format!("Keyed service {sid} observation is closed"),
                        ));
                    }
                    Ok(())
                }));
                handler(view, context)
            }),
        )?));

        Ok(Arc::new(move || {
            if !stopped.swap(true, Ordering::AcqRel) {
                if let Some(unsub) = stop.lock().unwrap().take() {
                    unsub.call();
                }
            }
            Box::pin(futures::future::ready(Ok(())))
        }))
    }

    fn bind_singleton(&self, service_id: &str, target: ServiceObject) {
        let slot = self.inner.singletons.lock().unwrap().get(service_id).cloned();
        if let Some(slot) = slot {
            slot.bind(target);
        }
    }

    fn bind_keyed(&self, service_id: &str, services: KeyedServiceSource) {
        self.inner
            .keyed_sources
            .lock()
            .unwrap()
            .insert(service_id.to_string(), services);
    }

    fn dispose(&self) {
        let slots: Vec<ServiceSlot> = {
            let mut singletons = self.inner.singletons.lock().unwrap();
            let slots = singletons.values().cloned().collect();
            singletons.clear();
            slots
        };
        self.inner.keyed_sources.lock().unwrap().clear();
        for slot in slots {
            slot.unbind();
        }
    }
}

struct StagedServiceInstance {
    implementation: ServiceObject,
    release: Option<FallibleRelease>,
}

#[derive(Clone)]
struct StagedServiceSpawner {
    inner: Arc<StagedServiceSpawnerInner>,
}

struct StagedServiceSpawnerInner {
    lifecycle: FacetLifecycle,
    validate: Arc<dyn Fn(&str, &ServiceObject) -> pi_js::Result<()> + Send + Sync>,
    instances: Mutex<IndexMap<String, StagedServiceInstance>>,
    installer: Mutex<Option<ServiceInstanceInstaller>>,
}

impl StagedServiceSpawner {
    fn new(
        lifecycle: FacetLifecycle,
        validate: Arc<dyn Fn(&str, &ServiceObject) -> pi_js::Result<()> + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(StagedServiceSpawnerInner {
                lifecycle,
                validate,
                instances: Mutex::new(IndexMap::new()),
                installer: Mutex::new(None),
            }),
        }
    }

    fn connect(&self, installer: ServiceInstanceInstaller) -> pi_js::Result<()> {
        let staged: Vec<(String, ServiceObject)> = {
            let mut guard = self.inner.installer.lock().unwrap();
            if guard.is_some() {
                return Err(pi_js::Error::js("Error", "Facet service provider is already connected"));
            }
            *guard = Some(Arc::clone(&installer));
            self.inner
                .instances
                .lock()
                .unwrap()
                .iter()
                .map(|(k, inst)| (k.clone(), inst.implementation.clone()))
                .collect()
        };

        for (key, implementation) in staged {
            let release = installer(&key, implementation)?;
            let mut instances = self.inner.instances.lock().unwrap();
            if let Some(inst) = instances.get_mut(&key) {
                inst.release = Some(release);
            } else {
                drop(instances);
                release()?;
            }
        }
        Ok(())
    }

    fn spawn(&self, key: &str, implementation: ServiceObject) -> pi_js::Result<pi_js::Unsubscribe> {
        self.inner.lifecycle.assert_active("spawn service instances")?;
        (self.inner.validate)(key, &implementation)?;
        {
            let mut instances = self.inner.instances.lock().unwrap();
            if instances.contains_key(key) {
                return Err(pi_js::Error::js(
                    "Error",
                    format!("Facet service already has a live instance with key {key}"),
                ));
            }
            instances.insert(
                key.to_string(),
                StagedServiceInstance {
                    implementation: implementation.clone(),
                    release: None,
                },
            );
        }

        let installer = self.inner.installer.lock().unwrap().clone();
        if let Some(installer) = installer {
            match installer(key, implementation) {
                Ok(release) => {
                    let mut instances = self.inner.instances.lock().unwrap();
                    if let Some(inst) = instances.get_mut(key) {
                        inst.release = Some(release);
                    } else {
                        drop(instances);
                        release()?;
                    }
                }
                Err(error) => {
                    self.inner.instances.lock().unwrap().shift_remove(key);
                    return Err(error);
                }
            }
        }

        let spawner = self.clone();
        let key_owned = key.to_string();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_for_unsub = Arc::clone(&closed);
        let spawner_for_unsub = spawner.clone();
        let key_for_unsub = key_owned.clone();

        let close_fn: FallibleRelease = Arc::new(move || {
            if closed_for_unsub.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let release = {
                let mut instances = spawner_for_unsub.inner.instances.lock().unwrap();
                instances.shift_remove(&key_for_unsub).and_then(|inst| inst.release)
            };
            if let Some(release) = release {
                release()?;
            }
            Ok(())
        });

        let own_close = Arc::clone(&close_fn);
        self.inner.lifecycle.own(Arc::new(move || {
            let res = own_close();
            Box::pin(futures::future::ready(res))
        }))?;

        Ok(pi_js::Unsubscribe::new(move || {
            let _ = close_fn();
        }))
    }
}

#[derive(Clone)]
enum FacetProvision {
    Singleton {
        service: Service,
        implementation: ServiceObject,
        install: Arc<dyn Fn(&RemoteServiceProvider) -> pi_js::Result<()> + Send + Sync>,
        validate_replacement: Arc<dyn Fn(&RemoteServiceProvider) -> pi_js::Result<()> + Send + Sync>,
        replace: Arc<dyn Fn(&RemoteServiceProvider) -> pi_js::Result<()> + Send + Sync>,
    },
    Keyed {
        service: Service,
        connect_local: Arc<dyn Fn(&LocalKeyedServiceRegistry) -> pi_js::Result<()> + Send + Sync>,
        connect_remote: Arc<dyn Fn(&RemoteServiceProvider) -> pi_js::Result<()> + Send + Sync>,
    },
}

impl FacetProvision {
    fn service(&self) -> &Service {
        match self {
            Self::Singleton { service, .. } => service,
            Self::Keyed { service, .. } => service,
        }
    }

    fn mode(&self) -> ServiceMode {
        match self {
            Self::Singleton { .. } => ServiceMode::Singleton,
            Self::Keyed { .. } => ServiceMode::Keyed,
        }
    }
}

#[derive(Clone)]
struct FacetRuntime {
    facet_id: String,
    lifecycle: FacetLifecycle,
    state: Arc<Mutex<FacetRuntimeMutable>>,
}

struct FacetRuntimeMutable {
    requires: Vec<FacetServiceReference>,
    provides: Vec<FacetServiceReference>,
    provisions: Vec<FacetProvision>,
    singleton_views: IndexMap<String, ServiceObject>,
}

impl FacetRuntime {
    fn shape(&self) -> FacetShape {
        let state = self.state.lock().unwrap();
        FacetShape {
            facet_id: self.facet_id.clone(),
            requires: state.requires.clone(),
            provides: state.provides.clone(),
        }
    }
}

/// Private lifecycle and dependency kernel behind the atomic host entry point.
#[derive(Clone)]
pub struct FacetKernel {
    inner: Arc<FacetKernelInner>,
}

struct FacetKernelInner {
    initial_facets: Vec<Facet>,
    service_sources: Vec<RemoteServiceSource>,
    on_error: ErrorReporter,
    facets: Mutex<IndexMap<String, FacetRuntime>>,
    service_slots: HostServiceSlots,
    // PORT: JS Map identity key. `RemoteServiceSource` is `Eq + Hash` by Arc identity.
    source_bindings: Mutex<IndexMap<RemoteServiceSource, RemoteServices>>,
    activation_order: Mutex<Vec<String>>,
    provider: Mutex<Option<RemoteServiceProvider>>,
    internal_services: Mutex<Option<RemoteServices>>,
    local_keyed_services: Mutex<Option<LocalKeyedServiceRegistry>>,
    phase: Mutex<GenerationPhase>,
}

impl FacetKernel {
    pub fn new(options: FacetOptions) -> pi_js::Result<Self> {
        let mut seen = IndexSet::new();
        for facet in &options.facets {
            if facet.id().is_empty() {
                return Err(pi_js::Error::js("Error", "Facet ID must not be empty"));
            }
            if !seen.insert(facet.id().to_string()) {
                return Err(pi_js::Error::js(
                    "Error",
                    "Facet IDs must be unique within a generation",
                ));
            }
        }
        Ok(Self {
            inner: Arc::new(FacetKernelInner {
                initial_facets: options.facets,
                service_sources: options.service_sources.unwrap_or_default(),
                on_error: options.on_error.unwrap_or_else(|| Arc::new(|_| {})),
                facets: Mutex::new(IndexMap::new()),
                service_slots: HostServiceSlots::new(),
                source_bindings: Mutex::new(IndexMap::new()),
                activation_order: Mutex::new(Vec::new()),
                provider: Mutex::new(None),
                internal_services: Mutex::new(None),
                local_keyed_services: Mutex::new(None),
                phase: Mutex::new(GenerationPhase::Setup),
            }),
        })
    }

    pub fn provider(&self) -> pi_js::Result<RemoteServiceProvider> {
        self.inner
            .provider
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| pi_js::Error::js("Error", "Facet service provider is not assembled"))
    }

    fn create_facet_runtime(&self, facet_id: &str) -> FacetRuntime {
        FacetRuntime {
            facet_id: facet_id.to_string(),
            lifecycle: FacetLifecycle::new(facet_id),
            state: Arc::new(Mutex::new(FacetRuntimeMutable {
                requires: Vec::new(),
                provides: Vec::new(),
                provisions: Vec::new(),
                singleton_views: IndexMap::new(),
            })),
        }
    }

    fn setup_facet(&self, facet: &Facet, record: &FacetRuntime) -> pi_js::Result<()> {
        let env = self.environment(record);
        facet.setup(&env)?;
        record.lifecycle.prepared()
    }

    pub async fn activate(&self) -> pi_js::Result<()> {
        let outcome: pi_js::Result<()> = async {
            let mut records = Vec::new();
            for facet in &self.inner.initial_facets {
                let record = self.create_facet_runtime(facet.id());
                self.inner
                    .facets
                    .lock()
                    .unwrap()
                    .insert(facet.id().to_string(), record.clone());
                self.setup_facet(facet, &record)?;
                records.push(record);
            }

            *self.inner.phase.lock().unwrap() = GenerationPhase::Assembling;
            let shapes: Vec<FacetShape> = records.iter().map(FacetRuntime::shape).collect();
            let external_services = self.resolve_external_services(&shapes).await?;
            let order = validate_facets(&shapes, &external_services)?;
            *self.inner.activation_order.lock().unwrap() = order.clone();
            self.assemble_providers()?;
            self.bind_services(&external_services)?;

            *self.inner.phase.lock().unwrap() = GenerationPhase::Connecting;
            let mut readiness_targets: Vec<RemoteServices> =
                self.inner.source_bindings.lock().unwrap().values().cloned().collect();
            readiness_targets.push(self.internal_service_binding()?);
            futures::future::try_join_all(
                readiness_targets
                    .into_iter()
                    .map(|services| async move { services.ready(BACKGROUND_CONTEXT.clone()).await }),
            )
            .await?;

            *self.inner.phase.lock().unwrap() = GenerationPhase::Activating;
            for id in &order {
                let record = self.inner.facets.lock().unwrap().get(id).cloned();
                if let Some(record) = record {
                    record.lifecycle.activate().await?;
                }
            }
            *self.inner.phase.lock().unwrap() = GenerationPhase::Active;
            Ok(())
        }
        .await;

        if let Err(error) = outcome {
            let cleanup_errors = self.terminate(None).await;
            if !cleanup_errors.is_empty() {
                let mut all = vec![error];
                all.extend(cleanup_errors);
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Facet generation startup and cleanup failed".to_string(),
                        errors: all,
                    },
                ));
            }
            return Err(error);
        }
        Ok(())
    }

    pub async fn reload(&self, facets: Vec<Facet>) -> pi_js::Result<()> {
        {
            let mut phase = self.inner.phase.lock().unwrap();
            if *phase != GenerationPhase::Active {
                return Err(pi_js::Error::js(
                    "Error",
                    format!("Facet host cannot reload while {}", phase.as_str()),
                ));
            }
            let mut seen = IndexSet::new();
            for facet in &facets {
                if facet.id().is_empty() {
                    return Err(pi_js::Error::js("Error", "Facet ID must not be empty"));
                }
                if !seen.insert(facet.id().to_string()) {
                    return Err(pi_js::Error::js("Error", "Reloaded facet IDs must be unique"));
                }
            }
            let existing = self.inner.facets.lock().unwrap();
            for facet in &facets {
                if !existing.contains_key(facet.id()) {
                    return Err(pi_js::Error::js("Error", format!("Facet {} is not active", facet.id())));
                }
            }
            *phase = GenerationPhase::Reloading;
        }

        let mut staged = Vec::new();
        let mut candidates = Vec::new();
        let setup_res: pi_js::Result<()> = (|| {
            for facet in &facets {
                let record = self.create_facet_runtime(facet.id());
                staged.push(record.clone());
                self.setup_facet(facet, &record)?;
                let prev_shape = {
                    let existing = self.inner.facets.lock().unwrap();
                    existing.get(facet.id()).map(FacetRuntime::shape)
                };
                if let Some(prev_shape) = prev_shape {
                    if !same_facet_shape(&prev_shape, &record.shape()) {
                        return Err(pi_js::Error::js(
                            "Error",
                            format!(
                                "Reloaded facet {} must preserve its service requirements and provisions",
                                facet.id()
                            ),
                        ));
                    }
                }
                let provisions = record.state.lock().unwrap().provisions.clone();
                self.validate_replacement_provisions(&provisions)?;
                candidates.push(record);
            }
            Ok(())
        })();

        if let Err(error) = setup_res {
            staged.reverse();
            let cleanup_errors = dispose_facet_records(staged).await;
            if !cleanup_errors.is_empty() {
                let abort_errors = self.abort(None).await;
                let mut all = vec![error];
                all.extend(cleanup_errors);
                all.extend(abort_errors);
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Facet reload setup and cleanup failed".to_string(),
                        errors: all,
                    },
                ));
            }
            *self.inner.phase.lock().unwrap() = GenerationPhase::Active;
            return Err(error);
        }

        let mut replacements: IndexMap<String, FacetRuntime> =
            candidates.into_iter().map(|r| (r.facet_id.clone(), r)).collect();
        let activation_order = self.inner.activation_order.lock().unwrap().clone();
        let candidate_order: Vec<FacetRuntime> = activation_order
            .iter()
            .filter_map(|id| replacements.shift_remove(id))
            .collect();

        let activate_res: pi_js::Result<()> = async {
            for candidate in &candidate_order {
                candidate.lifecycle.activate().await?;
            }
            for candidate in &candidate_order {
                let provisions = candidate.state.lock().unwrap().provisions.clone();
                self.validate_replacement_provisions(&provisions)?;
            }
            Ok(())
        }
        .await;

        if let Err(error) = activate_res {
            let mut to_dispose = candidate_order;
            to_dispose.reverse();
            let cleanup_errors = dispose_facet_records(to_dispose).await;
            if !cleanup_errors.is_empty() {
                let abort_errors = self.abort(None).await;
                let mut all = vec![error];
                all.extend(cleanup_errors);
                all.extend(abort_errors);
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Facet reload activation and cleanup failed".to_string(),
                        errors: all,
                    },
                ));
            }
            *self.inner.phase.lock().unwrap() = GenerationPhase::Active;
            return Err(error);
        }

        let previous: Vec<FacetRuntime> = {
            let mut facets_map = self.inner.facets.lock().unwrap();
            let mut previous = Vec::with_capacity(candidate_order.len());
            for candidate in &candidate_order {
                if let Some(prev) = facets_map.insert(candidate.facet_id.clone(), candidate.clone()) {
                    previous.push(prev);
                }
            }
            previous
        };

        let previous_for_abort = previous.clone();
        let cutover_res: pi_js::Result<()> = async {
            let provider = self.provider()?;
            for candidate in &candidate_order {
                let provisions = candidate.state.lock().unwrap().provisions.clone();
                for provision in provisions {
                    if let FacetProvision::Singleton {
                        service,
                        implementation,
                        replace,
                        ..
                    } = provision
                    {
                        if service.local {
                            self.inner.service_slots.bind_singleton(&service.id, implementation);
                        } else {
                            replace(&provider)?;
                        }
                    }
                }
            }

            let mut reversed_prev = previous;
            reversed_prev.reverse();
            let retirement_errors = dispose_facet_records(reversed_prev).await;
            crate::services::errors::throw_collected_errors(&retirement_errors, "Failed to retire replaced facets")?;

            let local_registry = self.local_keyed_registry()?;
            for candidate in &candidate_order {
                let provisions = candidate.state.lock().unwrap().provisions.clone();
                for provision in provisions {
                    if let FacetProvision::Keyed {
                        service,
                        connect_local,
                        connect_remote,
                    } = provision
                    {
                        if service.local {
                            connect_local(&local_registry)?;
                        } else {
                            connect_remote(&provider)?;
                        }
                    }
                }
            }
            Ok(())
        }
        .await;

        if let Err(error) = cutover_res {
            let abort_errors = self.abort(Some(previous_for_abort)).await;
            let mut all = vec![error];
            all.extend(abort_errors);
            return Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Facet reload failed after cutover".to_string(),
                    errors: all,
                },
            ));
        }

        *self.inner.phase.lock().unwrap() = GenerationPhase::Active;
        Ok(())
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        {
            let phase = *self.inner.phase.lock().unwrap();
            if phase == GenerationPhase::Dead {
                return Ok(());
            }
            if phase != GenerationPhase::Active {
                return Err(pi_js::Error::js(
                    "Error",
                    format!("Facet host cannot be disposed while {}", phase.as_str()),
                ));
            }
        }
        let errors = self.terminate(None).await;
        crate::services::errors::throw_collected_errors(&errors, "Failed to dispose facet generation")
    }

    fn validate_replacement_provisions(&self, provisions: &[FacetProvision]) -> pi_js::Result<()> {
        let provider = self.provider()?;
        for provision in provisions {
            if let FacetProvision::Singleton {
                service,
                validate_replacement,
                ..
            } = provision
            {
                if !service.local {
                    validate_replacement(&provider)?;
                }
            }
        }
        Ok(())
    }

    fn environment(&self, runtime: &FacetRuntime) -> FacetEnvironment {
        let lifecycle_provide = runtime.lifecycle.clone();
        let runtime_provide = runtime.clone();
        let provide_fn: Arc<dyn Fn(&Service, ServiceObject) -> pi_js::Result<()> + Send + Sync> =
            Arc::new(move |service, implementation| {
                lifecycle_provide.assert_setting_up("provide services")?;
                let mut state = runtime_provide.state.lock().unwrap();
                record_service_reference(&mut state.provides, service, ServiceMode::Singleton);
                let svc_install = service.clone();
                let impl_install = implementation.clone();
                let svc_validate = service.clone();
                let impl_validate = implementation.clone();
                let svc_replace = service.clone();
                let impl_replace = implementation.clone();
                state.provisions.push(FacetProvision::Singleton {
                    service: service.clone(),
                    implementation,
                    install: Arc::new(move |provider| provider.provide(&svc_install, Arc::new(impl_install.clone()))),
                    validate_replacement: Arc::new(move |provider| {
                        provider.validate_replacement(&svc_validate, &impl_validate)
                    }),
                    replace: Arc::new(move |provider| provider.replace(&svc_replace, Arc::new(impl_replace.clone()))),
                });
                Ok(())
            });

        let lifecycle_pm = runtime.lifecycle.clone();
        let runtime_pm = runtime.clone();
        let provide_many_fn: Arc<dyn Fn(&Service) -> pi_js::Result<ServiceSpawnerFn> + Send + Sync> =
            Arc::new(move |service| {
                lifecycle_pm.assert_setting_up("provide service instances")?;
                let mut state = runtime_pm.state.lock().unwrap();
                record_service_reference(&mut state.provides, service, ServiceMode::Keyed);

                let svc_for_validate = service.clone();
                let validate: Arc<dyn Fn(&str, &ServiceObject) -> pi_js::Result<()> + Send + Sync> =
                    Arc::new(move |key, implementation| {
                        if key.is_empty() {
                            return Err(pi_js::Error::js(
                                "TypeError",
                                "Facet service instance key must not be empty",
                            ));
                        }
                        if !is_plain_object(implementation) {
                            return Err(pi_js::Error::js(
                                "TypeError",
                                format!("Facet service {} implementation must be an object", svc_for_validate.id),
                            ));
                        }
                        if !svc_for_validate.local {
                            validate_remote_service_implementation(&svc_for_validate.id, implementation)?;
                        }
                        Ok(())
                    });

                let instances = StagedServiceSpawner::new(lifecycle_pm.clone(), validate);
                let inst_local = instances.clone();
                let svc_local = service.clone();
                let inst_remote = instances.clone();
                let svc_remote = service.clone();
                state.provisions.push(FacetProvision::Keyed {
                    service: service.clone(),
                    connect_local: Arc::new(move |registry| {
                        let registry = registry.clone();
                        let service = svc_local.clone();
                        inst_local.connect(Arc::new(move |key, implementation| {
                            let unsub = Mutex::new(Some(registry.spawn(&service, key, implementation)?));
                            Ok(Arc::new(move || {
                                if let Some(u) = unsub.lock().unwrap().take() {
                                    u.call();
                                }
                                Ok(())
                            }))
                        }))
                    }),
                    connect_remote: Arc::new(move |provider| {
                        let provider = provider.clone();
                        let service = svc_remote.clone();
                        inst_remote.connect(Arc::new(move |key, implementation| {
                            provider.spawn_with_fallible_close(&service, key, Arc::new(implementation))
                        }))
                    }),
                });

                Ok(Arc::new(move |key, implementation| {
                    instances.spawn(key, implementation)
                }))
            });

        let lifecycle_use = runtime.lifecycle.clone();
        let runtime_use = runtime.clone();
        let weak_kernel_use = Arc::downgrade(&self.inner);
        let use_fn: Arc<dyn Fn(&Service) -> pi_js::Result<ServiceObject> + Send + Sync> = Arc::new(move |service| {
            lifecycle_use.assert_setting_up("acquire services")?;
            let kernel = weak_kernel_use
                .upgrade()
                .ok_or_else(|| pi_js::Error::js("Error", "Facet host has been dropped"))?;
            let mut state = runtime_use.state.lock().unwrap();
            record_service_reference(&mut state.requires, service, ServiceMode::Singleton);
            if let Some(existing) = state.singleton_views.get(&service.id) {
                return Ok(existing.clone());
            }
            let lc = lifecycle_use.clone();
            let view = kernel
                .service_slots
                .get_singleton(service, Arc::new(move || lc.assert_service_access()));
            state.singleton_views.insert(service.id.clone(), view.clone());
            Ok(view)
        });

        let lifecycle_obs = runtime.lifecycle.clone();
        let runtime_obs = runtime.clone();
        let weak_kernel_obs = Arc::downgrade(&self.inner);
        let observe_fn: Arc<dyn Fn(&Service, ObserveHandler) -> pi_js::Result<()> + Send + Sync> =
            Arc::new(move |service, handler| {
                lifecycle_obs.assert_setting_up("observe services")?;
                let mut state = runtime_obs.state.lock().unwrap();
                record_service_reference(&mut state.requires, service, ServiceMode::Keyed);
                drop(state);

                let service_owned = service.clone();
                let lc_for_start = lifecycle_obs.clone();
                let kernel_for_start = Weak::clone(&weak_kernel_obs);
                lifecycle_obs.observe(Arc::new(move || {
                    let kernel = kernel_for_start
                        .upgrade()
                        .ok_or_else(|| pi_js::Error::js("Error", "Facet host has been dropped"))?;
                    let lc_access = lc_for_start.clone();
                    kernel.service_slots.observe(
                        &service_owned,
                        Arc::new(move || lc_access.assert_service_access()),
                        Arc::clone(&handler),
                    )
                }))
            });

        let lc_rs = runtime.lifecycle.clone();
        let lc_own = runtime.lifecycle.clone();
        let lc_act = runtime.lifecycle.clone();
        let lc_deact = runtime.lifecycle.clone();

        FacetEnvironment::from_handlers(FacetEnvironmentHandlers {
            use_fn,
            observe_fn,
            provide_fn,
            provide_many_fn,
            assert_replicated_state_fn: Arc::new(move || lc_rs.assert_running("create replicated state")),
            own_fn: Arc::new(move |disposal| lc_own.own(disposal)),
            on_activate_fn: Arc::new(move |callback| lc_act.on_activate(callback)),
            on_deactivate_fn: Arc::new(move |callback| lc_deact.own(callback)),
        })
    }

    async fn resolve_external_services(
        &self,
        records: &[FacetShape],
    ) -> pi_js::Result<IndexMap<String, ExternalService>> {
        let catalogues = futures::future::try_join_all(self.inner.service_sources.iter().map(|source| {
            let source = source.clone();
            async move {
                let entries = source.catalogue(BACKGROUND_CONTEXT.clone()).await?;
                Ok::<_, pi_js::Error>((source, entries))
            }
        }))
        .await?;

        let mut offered: IndexMap<String, (ServiceMode, RemoteServiceSource)> = IndexMap::new();
        for (source, entries) in catalogues {
            for entry in entries {
                if offered.contains_key(&entry.service_id) {
                    return Err(pi_js::Error::js(
                        "Error",
                        format!(
                            "Facet host service {} is offered by more than one source",
                            entry.service_id
                        ),
                    ));
                }
                offered.insert(entry.service_id, (entry.mode, source.clone()));
            }
        }

        let local: IndexSet<String> = records
            .iter()
            .flat_map(|r| r.provides.iter().map(|p| p.service_id.clone()))
            .collect();

        let mut external: IndexMap<String, ExternalService> = IndexMap::new();
        for record in records {
            for requirement in &record.requires {
                if local.contains(&requirement.service_id) || external.contains_key(&requirement.service_id) {
                    continue;
                }
                let mut source = offered.get(&requirement.service_id).cloned();
                if source.is_none() {
                    let deferred: Vec<&RemoteServiceSource> = self
                        .inner
                        .service_sources
                        .iter()
                        .filter(|s| s.accepts_unavailable_services())
                        .collect();
                    if deferred.len() > 1 {
                        return Err(pi_js::Error::js(
                            "Error",
                            format!(
                                "Facet host service {} has more than one deferred source",
                                requirement.service_id
                            ),
                        ));
                    }
                    if deferred.len() == 1 {
                        source = Some((requirement.mode, deferred[0].clone()));
                    }
                }
                if let Some((mode, src)) = source {
                    external.insert(
                        requirement.service_id.clone(),
                        ExternalService {
                            service: requirement.service.clone(),
                            mode,
                            source: src,
                        },
                    );
                }
            }
        }

        let mut service_ids_by_source: IndexMap<RemoteServiceSource, Vec<String>> = IndexMap::new();
        for (service_id, ext) in &external {
            service_ids_by_source
                .entry(ext.source.clone())
                .or_default()
                .push(service_id.clone());
        }

        let assert_access = self.make_target_access_check();
        let mut opened = IndexMap::new();
        for (source, service_ids) in service_ids_by_source {
            let services = source.open(RemoteServiceSourceOpenOptions {
                services: service_ids.into_iter().map(|id| ServiceIdRef { id }).collect(),
                assert_access: Arc::clone(&assert_access),
                on_error: Arc::clone(&self.inner.on_error),
            });
            opened.insert(source, services);
        }
        *self.inner.source_bindings.lock().unwrap() = opened;

        Ok(external)
    }

    fn assemble_providers(&self) -> pi_js::Result<()> {
        let provisions = self.provisions();
        let remote_entries: Vec<RemoteServiceProviderEntry> = provisions
            .iter()
            .filter(|p| !p.service().local)
            .map(|p| RemoteServiceProviderEntry::Definition {
                service: p.service().clone(),
                mode: p.mode(),
            })
            .collect();
        let provider = RemoteServiceProvider::new(remote_entries)?;

        let remote_service_ids: Vec<ServiceIdRef> = provisions
            .iter()
            .filter(|p| !p.service().local)
            .map(|p| ServiceIdRef {
                id: p.service().id.clone(),
            })
            .collect();
        let internal_binding =
            RemoteServiceBinding::from_impl(RemoteServiceBindingImpl::new(RemoteServiceBindingOptions {
                services: remote_service_ids,
                transport: create_loopback_service_transport(&provider),
                bound: None,
                on_error: Some(Arc::clone(&self.inner.on_error)),
                assert_access: Some(self.make_target_access_check()),
            })?);
        let internal_services = internal_binding.as_remote_services();

        let local_keyed: Vec<Service> = provisions
            .iter()
            .filter(|p| matches!(p, FacetProvision::Keyed { service, .. } if service.local))
            .map(|p| p.service().clone())
            .collect();
        let local_keyed_services = LocalKeyedServiceRegistry::new(&local_keyed, Arc::clone(&self.inner.on_error))?;

        *self.inner.provider.lock().unwrap() = Some(provider.clone());
        *self.inner.internal_services.lock().unwrap() = Some(internal_services);
        *self.inner.local_keyed_services.lock().unwrap() = Some(local_keyed_services.clone());

        for provision in &provisions {
            match provision {
                FacetProvision::Singleton { service, install, .. } => {
                    if !service.local {
                        install(&provider)?;
                    }
                }
                FacetProvision::Keyed {
                    service,
                    connect_local,
                    connect_remote,
                } => {
                    if service.local {
                        connect_local(&local_keyed_services)?;
                    } else {
                        connect_remote(&provider)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn bind_services(&self, external_services: &IndexMap<String, ExternalService>) -> pi_js::Result<()> {
        let internal_binding = self.internal_service_binding()?;
        let local_registry = self.local_keyed_registry()?;
        for provision in self.provisions() {
            match provision {
                FacetProvision::Singleton {
                    service,
                    implementation,
                    ..
                } => {
                    if !self.inner.service_slots.has_singleton(&service.id) {
                        continue;
                    }
                    let target = if service.local {
                        implementation
                    } else {
                        internal_binding.r#use::<ServiceObject>(&service)?
                    };
                    self.inner.service_slots.bind_singleton(&service.id, target);
                }
                FacetProvision::Keyed { service, .. } => {
                    let source = if service.local {
                        KeyedServiceSource::Local(local_registry.clone())
                    } else {
                        KeyedServiceSource::Remote(internal_binding.clone())
                    };
                    self.inner.service_slots.bind_keyed(&service.id, source);
                }
            }
        }

        let source_bindings = self.inner.source_bindings.lock().unwrap().clone();
        for (service_id, ext) in external_services {
            let Some(services) = source_bindings.get(&ext.source) else {
                return Err(pi_js::Error::js(
                    "Error",
                    format!("Service source for {service_id} is not open"),
                ));
            };
            if ext.mode == ServiceMode::Singleton {
                let target = services.r#use::<ServiceObject>(&ext.service)?;
                self.inner.service_slots.bind_singleton(service_id, target);
            } else {
                self.inner
                    .service_slots
                    .bind_keyed(service_id, KeyedServiceSource::Remote(services.clone()));
            }
        }
        Ok(())
    }

    fn provisions(&self) -> Vec<FacetProvision> {
        let facets = self.inner.facets.lock().unwrap();
        facets
            .values()
            .flat_map(|runtime| runtime.state.lock().unwrap().provisions.clone())
            .collect()
    }

    fn local_keyed_registry(&self) -> pi_js::Result<LocalKeyedServiceRegistry> {
        self.inner
            .local_keyed_services
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| pi_js::Error::js("Error", "Facet keyed services are not assembled"))
    }

    fn internal_service_binding(&self) -> pi_js::Result<RemoteServices> {
        self.inner
            .internal_services
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| pi_js::Error::js("Error", "Facet remote services are not assembled"))
    }

    async fn dispose_service_bindings(&self) -> Vec<pi_js::Error> {
        let mut bindings: Vec<RemoteServices> = {
            let mut guard = self.inner.source_bindings.lock().unwrap();
            let drained = std::mem::take(&mut *guard);
            drained.into_values().collect()
        };
        if let Some(internal) = self.inner.internal_services.lock().unwrap().take() {
            bindings.push(internal);
        }
        let results = futures::future::join_all(
            bindings
                .into_iter()
                .map(|services| async move { services.dispose(BACKGROUND_CONTEXT.clone()).await }),
        )
        .await;
        results.into_iter().filter_map(Result::err).collect()
    }

    fn make_target_access_check(&self) -> AssertAccess {
        let weak = Arc::downgrade(&self.inner);
        Arc::new(move || {
            let Some(inner) = weak.upgrade() else {
                return Err(pi_js::Error::js(
                    "Error",
                    "Facet service targets cannot be used during dead",
                ));
            };
            assert_target_access_phase(*inner.phase.lock().unwrap())
        })
    }

    async fn abort(&self, extra_records: Option<Vec<FacetRuntime>>) -> Vec<pi_js::Error> {
        {
            let facets = self.inner.facets.lock().unwrap();
            for record in facets.values() {
                record.lifecycle.revoke();
            }
        }
        if let Some(extra) = &extra_records {
            for record in extra {
                record.lifecycle.revoke();
            }
        }
        self.terminate(extra_records).await
    }

    async fn terminate(&self, extra_records: Option<Vec<FacetRuntime>>) -> Vec<pi_js::Error> {
        *self.inner.phase.lock().unwrap() = GenerationPhase::Disposing;
        let mut errors = self.dispose_lifecycles().await;
        if let Some(mut extra) = extra_records {
            extra.reverse();
            errors.extend(dispose_facet_records(extra).await);
        }

        if let Some(local) = self.inner.local_keyed_services.lock().unwrap().take() {
            local.dispose();
        }

        errors.extend(self.dispose_service_bindings().await);
        self.inner.service_slots.dispose();

        if let Some(provider) = self.inner.provider.lock().unwrap().as_ref() {
            if let Err(error) = provider.dispose() {
                errors.push(error);
            }
        }

        *self.inner.phase.lock().unwrap() = GenerationPhase::Dead;
        errors
    }

    async fn dispose_lifecycles(&self) -> Vec<pi_js::Error> {
        let records: Vec<FacetRuntime> = {
            let mut facets = self.inner.facets.lock().unwrap();
            let activation_order = self.inner.activation_order.lock().unwrap().clone();
            let order: Vec<String> = if activation_order.is_empty() {
                facets.keys().rev().cloned().collect()
            } else {
                activation_order.into_iter().rev().collect()
            };
            let mut records = Vec::new();
            for id in order {
                if let Some(record) = facets.shift_remove(&id) {
                    records.push(record);
                }
            }
            records
        };
        dispose_facet_records(records).await
    }
}

fn assert_target_access_phase(phase: GenerationPhase) -> pi_js::Result<()> {
    match phase {
        GenerationPhase::Activating
        | GenerationPhase::Active
        | GenerationPhase::Reloading
        | GenerationPhase::Disposing => Ok(()),
        _ => Err(pi_js::Error::js(
            "Error",
            format!("Facet service targets cannot be used during {}", phase.as_str()),
        )),
    }
}

fn is_plain_object(obj: &ServiceObject) -> bool {
    !obj.is_callable() && obj.as_context().is_none() && obj.to_json().is_none_or(|json| json.is_object())
}

async fn dispose_facet_records(records: Vec<FacetRuntime>) -> Vec<pi_js::Error> {
    let mut errors = Vec::new();
    for record in records {
        if let Err(error) = record.lifecycle.dispose().await {
            errors.push(error);
        }
    }
    errors
}

fn validate_facets(
    records: &[FacetShape],
    external_services: &IndexMap<String, ExternalService>,
) -> pi_js::Result<Vec<String>> {
    struct ProviderEntry {
        facet_id: Option<String>,
        mode: Option<ServiceMode>,
    }

    let mut providers: IndexMap<String, ProviderEntry> = IndexMap::new();
    for (service_id, ext) in external_services {
        providers.insert(
            service_id.clone(),
            ProviderEntry {
                facet_id: None,
                mode: Some(ext.mode),
            },
        );
    }

    for record in records {
        for provision in &record.provides {
            if let Some(existing) = providers.get(&provision.service_id) {
                if let Some(existing_mode) = existing.mode {
                    if existing_mode != provision.mode {
                        return Err(pi_js::Error::js(
                            "Error",
                            format!(
                                "Service {} is provided as both singleton and keyed",
                                provision.service_id
                            ),
                        ));
                    }
                }
                let Some(existing_facet) = &existing.facet_id else {
                    return Err(pi_js::Error::js(
                        "Error",
                        format!(
                            "Service {} is provided by both the host and {}",
                            provision.service_id, record.facet_id
                        ),
                    ));
                };
                return Err(pi_js::Error::js(
                    "Error",
                    format!(
                        "Service {} is provided by both {} and {}",
                        provision.service_id, existing_facet, record.facet_id
                    ),
                ));
            }
            providers.insert(
                provision.service_id.clone(),
                ProviderEntry {
                    facet_id: Some(record.facet_id.clone()),
                    mode: Some(provision.mode),
                },
            );
        }
    }

    let mut dependencies: IndexMap<String, IndexSet<String>> = records
        .iter()
        .map(|record| (record.facet_id.clone(), IndexSet::new()))
        .collect();
    let mut dependents: IndexMap<String, IndexSet<String>> = records
        .iter()
        .map(|record| (record.facet_id.clone(), IndexSet::new()))
        .collect();

    for record in records {
        for requirement in &record.requires {
            let Some(provider) = providers.get(&requirement.service_id) else {
                return Err(pi_js::Error::js(
                    "Error",
                    format!(
                        "Facet {} requires local/{}/{}, but no facet provides it",
                        record.facet_id,
                        requirement.service_id,
                        mode_str(requirement.mode),
                    ),
                ));
            };
            if let Some(provider_mode) = provider.mode {
                if provider_mode != requirement.mode {
                    let provider_name = provider.facet_id.as_deref().unwrap_or("the host");
                    return Err(pi_js::Error::js(
                        "Error",
                        format!(
                            "Facet {} requires {} as {}, but {} provides it as {}",
                            record.facet_id,
                            requirement.service_id,
                            mode_str(requirement.mode),
                            provider_name,
                            mode_str(provider_mode),
                        ),
                    ));
                }
            }
            let Some(provider_facet_id) = &provider.facet_id else {
                continue;
            };
            if provider_facet_id == &record.facet_id {
                continue;
            }
            dependencies
                .get_mut(&record.facet_id)
                .expect("facet dependency entry")
                .insert(provider_facet_id.clone());
            dependents
                .get_mut(provider_facet_id)
                .expect("facet dependent entry")
                .insert(record.facet_id.clone());
        }
    }

    topological_order(records, &dependencies, &dependents)
}

fn topological_order(
    records: &[FacetShape],
    dependencies: &IndexMap<String, IndexSet<String>>,
    dependents: &IndexMap<String, IndexSet<String>>,
) -> pi_js::Result<Vec<String>> {
    let mut remaining: IndexMap<String, usize> =
        dependencies.iter().map(|(id, deps)| (id.clone(), deps.len())).collect();
    let mut ready: Vec<String> = records
        .iter()
        .map(|record| record.facet_id.clone())
        .filter(|id| remaining.get(id).copied() == Some(0))
        .collect();
    let mut order = Vec::new();

    while !ready.is_empty() {
        let id = ready.remove(0);
        order.push(id.clone());
        if let Some(deps) = dependents.get(&id) {
            for dependent in deps {
                let count = remaining.get(dependent).copied().unwrap_or(0).saturating_sub(1);
                remaining.insert(dependent.clone(), count);
                if count == 0 {
                    ready.push(dependent.clone());
                }
            }
        }
    }

    if order.len() != records.len() {
        let cycle: Vec<String> = records
            .iter()
            .filter(|record| remaining.get(&record.facet_id).copied().unwrap_or(0) > 0)
            .map(|record| record.facet_id.clone())
            .collect();
        return Err(pi_js::Error::js(
            "Error",
            format!("Facet dependency cycle: {}", cycle.join(", ")),
        ));
    }

    Ok(order)
}

fn record_service_reference(target: &mut Vec<FacetServiceReference>, service: &Service, mode: ServiceMode) {
    if target
        .iter()
        .any(|reference| reference.service_id == service.id && reference.mode == mode)
    {
        return;
    }
    target.push(FacetServiceReference {
        service_id: service.id.clone(),
        service: service.clone(),
        mode,
    });
}

fn same_facet_shape(left: &FacetShape, right: &FacetShape) -> bool {
    same_references(&left.requires, &right.requires) && same_references(&left.provides, &right.provides)
}

fn same_references(left: &[FacetServiceReference], right: &[FacetServiceReference]) -> bool {
    left.len() == right.len()
        && left.iter().all(|reference| {
            right
                .iter()
                .any(|other| other.service_id == reference.service_id && other.mode == reference.mode)
        })
}

fn mode_str(mode: ServiceMode) -> &'static str {
    match mode {
        ServiceMode::Singleton => "singleton",
        ServiceMode::Keyed => "keyed",
    }
}
