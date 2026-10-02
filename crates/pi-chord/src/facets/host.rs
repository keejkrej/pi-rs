//! Port of packages/chord/src/facets/host.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

use crate::services::handle::{ServiceObject, ServiceSlot};
use crate::services::instances::{InstanceDirectory, InstanceDirectoryEntry};
use crate::services::provider::RemoteServiceProvider;
use crate::types::{
    Context, Facet, FacetEnvironment, FacetOptions, RemoteServiceSource, RemoteServices, Service, ServiceMode,
};

type ErrorReporter = Arc<dyn Fn(pi_js::Error) + Send + Sync>;
type AssertAccess = Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>;
type Disposal = Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ActivateCallback = Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ObserveStart = Arc<dyn Fn() -> Disposal + Send + Sync>;
type ObserveHandler = Arc<dyn Fn(ServiceObject, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ServiceInstanceInstaller = Arc<dyn Fn(String, ServiceObject) -> pi_js::Unsubscribe + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum LifecycleState {
    #[serde(rename = "setting_up")]
    SettingUp,
    #[serde(rename = "prepared")]
    Prepared,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "disposing")]
    Disposing,
    #[serde(rename = "dead")]
    Dead,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum GenerationPhase {
    #[serde(rename = "setup")]
    Setup,
    #[serde(rename = "assembling")]
    Assembling,
    #[serde(rename = "connecting")]
    Connecting,
    #[serde(rename = "activating")]
    Activating,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "reloading")]
    Reloading,
    #[serde(rename = "disposing")]
    Disposing,
    #[serde(rename = "dead")]
    Dead,
}

struct FacetServiceReference {
    service_id: String,
    service: Service,
    mode: ServiceMode,
}

struct ExternalService {
    service: Service,
    mode: ServiceMode,
    source: RemoteServiceSource,
}

struct FacetShape {
    facet_id: String,
    requires: Vec<FacetServiceReference>,
    provides: Vec<FacetServiceReference>,
}

struct FacetLifecycle {
    inner: Arc<FacetLifecycleInner>,
}

struct FacetLifecycleInner {
    id: String,
    effects: Mutex<Vec<Disposal>>,
    observations: Mutex<Vec<ObserveStart>>,
    activate: Mutex<Vec<ActivateCallback>>,
    state: Mutex<LifecycleState>,
    service_access: Mutex<bool>,
}

impl Clone for FacetLifecycle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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
                service_access: Mutex::new(false),
            }),
        }
    }

    fn id(&self) -> &str {
        &self.inner.id
    }

    fn assert_setting_up(&self, operation: &str) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::assert_setting_up")
    }

    fn assert_running(&self, operation: &str) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::assert_running")
    }

    fn assert_active(&self, operation: &str) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::assert_active")
    }

    fn assert_service_access(&self) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::assert_service_access")
    }

    fn revoke(&self) {
        todo!("port: FacetLifecycle::revoke")
    }

    fn own(&self, disposal: Disposal) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::own")
    }

    fn observe(&self, start: ObserveStart) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::observe")
    }

    fn on_activate(&self, callback: ActivateCallback) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::on_activate")
    }

    fn prepared(&self) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::prepared")
    }

    async fn activate(&self) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::activate")
    }

    async fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: FacetLifecycle::dispose")
    }
}

#[derive(Clone)]
struct LocalKeyedRegistration {
    generations: Arc<Mutex<IndexMap<String, i64>>>,
    directory: InstanceDirectory<InstanceDirectoryEntry>,
}

struct LocalKeyedServiceRegistry {
    inner: Arc<LocalKeyedServiceRegistryInner>,
}

struct LocalKeyedServiceRegistryInner {
    registrations: Mutex<IndexMap<String, LocalKeyedRegistration>>,
    disposed: Mutex<bool>,
}

impl Clone for LocalKeyedServiceRegistry {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl LocalKeyedServiceRegistry {
    fn new(services: &[Service], on_error: ErrorReporter) -> pi_js::Result<Self> {
        todo!("port: LocalKeyedServiceRegistry::new")
    }

    fn spawn(&self, service: &Service, key: &str, implementation: ServiceObject) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: LocalKeyedServiceRegistry::spawn")
    }

    fn observe(&self, service: &Service, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: LocalKeyedServiceRegistry::observe")
    }

    fn dispose(&self) {
        todo!("port: LocalKeyedServiceRegistry::dispose")
    }

    fn registration(&self, service_id: &str) -> pi_js::Result<LocalKeyedRegistration> {
        todo!("port: LocalKeyedServiceRegistry::registration")
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        todo!("port: LocalKeyedServiceRegistry::assert_active")
    }
}

// PORT: structural `KeyedServiceSource`. Remote services are the types.ts handle, not this registry.
enum KeyedServiceSource {
    Local(LocalKeyedServiceRegistry),
    Remote(RemoteServices),
}

struct HostServiceSlots {
    inner: Arc<HostServiceSlotsInner>,
}

struct HostServiceSlotsInner {
    singletons: Mutex<IndexMap<String, ServiceSlot>>,
    keyed_sources: Mutex<IndexMap<String, KeyedServiceSource>>,
}

impl Clone for HostServiceSlots {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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
        todo!("port: HostServiceSlots::get_singleton")
    }

    fn has_singleton(&self, service_id: &str) -> bool {
        todo!("port: HostServiceSlots::has_singleton")
    }

    fn observe(
        &self,
        service: &Service,
        assert_access: AssertAccess,
        handler: ObserveHandler,
    ) -> pi_js::Result<Disposal> {
        todo!("port: HostServiceSlots::observe")
    }

    fn bind_singleton(&self, service_id: &str, target: ServiceObject) {
        todo!("port: HostServiceSlots::bind_singleton")
    }

    fn bind_keyed(&self, service_id: &str, services: KeyedServiceSource) {
        todo!("port: HostServiceSlots::bind_keyed")
    }

    fn dispose(&self) {
        todo!("port: HostServiceSlots::dispose")
    }
}

struct StagedServiceInstance {
    key: String,
    implementation: ServiceObject,
    release: Option<pi_js::Unsubscribe>,
}

struct StagedServiceSpawner {
    inner: Arc<StagedServiceSpawnerInner>,
}

struct StagedServiceSpawnerInner {
    lifecycle: FacetLifecycle,
    validate: Arc<dyn Fn(&str, &ServiceObject) -> pi_js::Result<()> + Send + Sync>,
    instances: Mutex<IndexMap<String, StagedServiceInstance>>,
    installer: Mutex<Option<ServiceInstanceInstaller>>,
}

impl Clone for StagedServiceSpawner {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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
        todo!("port: StagedServiceSpawner::connect")
    }

    fn spawn(&self, key: &str, implementation: ServiceObject) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: StagedServiceSpawner::spawn")
    }
}

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

struct FacetRuntime {
    shape: FacetShape,
    lifecycle: FacetLifecycle,
    provisions: Vec<FacetProvision>,
    singleton_views: IndexMap<String, ServiceObject>,
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
    // PORT: JS Map identity key. `RemoteServiceSource` must be `Eq + Hash` by identity.
    source_bindings: Mutex<IndexMap<RemoteServiceSource, RemoteServices>>,
    activation_order: Mutex<Vec<String>>,
    provider: Mutex<Option<RemoteServiceProvider>>,
    internal_services: Mutex<Option<RemoteServices>>,
    local_keyed_services: Mutex<Option<LocalKeyedServiceRegistry>>,
    phase: Mutex<GenerationPhase>,
}

impl FacetKernel {
    pub fn new(options: FacetOptions) -> pi_js::Result<Self> {
        todo!("port: FacetKernel::new")
    }

    pub fn provider(&self) -> pi_js::Result<RemoteServiceProvider> {
        todo!("port: FacetKernel::provider")
    }

    fn create_facet_runtime(&self, facet_id: &str) -> FacetRuntime {
        FacetRuntime {
            shape: FacetShape {
                facet_id: facet_id.to_string(),
                requires: Vec::new(),
                provides: Vec::new(),
            },
            lifecycle: FacetLifecycle::new(facet_id),
            provisions: Vec::new(),
            singleton_views: IndexMap::new(),
        }
    }

    fn setup_facet(&self, facet: &Facet, record: &mut FacetRuntime) -> pi_js::Result<()> {
        todo!("port: FacetKernel::setup_facet")
    }

    pub async fn activate(&self) -> pi_js::Result<()> {
        todo!("port: FacetKernel::activate")
    }

    pub async fn reload(&self, facets: Vec<Facet>) -> pi_js::Result<()> {
        todo!("port: FacetKernel::reload")
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: FacetKernel::dispose")
    }

    fn validate_replacement_provisions(&self, provisions: &[FacetProvision]) -> pi_js::Result<()> {
        todo!("port: FacetKernel::validate_replacement_provisions")
    }

    fn environment(&self, runtime: &mut FacetRuntime) -> FacetEnvironment {
        todo!("port: FacetKernel::environment")
    }

    async fn resolve_external_services(
        &self,
        records: &[FacetShape],
    ) -> pi_js::Result<IndexMap<String, ExternalService>> {
        todo!("port: FacetKernel::resolve_external_services")
    }

    fn assemble_providers(&self) -> pi_js::Result<()> {
        todo!("port: FacetKernel::assemble_providers")
    }

    fn bind_services(&self, external_services: &IndexMap<String, ExternalService>) -> pi_js::Result<()> {
        todo!("port: FacetKernel::bind_services")
    }

    fn provisions(&self) -> Vec<FacetProvision> {
        todo!("port: FacetKernel::provisions")
    }

    fn local_keyed_registry(&self) -> pi_js::Result<LocalKeyedServiceRegistry> {
        todo!("port: FacetKernel::local_keyed_registry")
    }

    fn internal_service_binding(&self) -> pi_js::Result<RemoteServices> {
        todo!("port: FacetKernel::internal_service_binding")
    }

    async fn dispose_service_bindings(&self) -> Vec<pi_js::Error> {
        todo!("port: FacetKernel::dispose_service_bindings")
    }

    fn assert_service_target_access(&self) -> pi_js::Result<()> {
        todo!("port: FacetKernel::assert_service_target_access")
    }

    async fn abort(&self, extra_records: Option<Vec<FacetRuntime>>) -> Vec<pi_js::Error> {
        todo!("port: FacetKernel::abort")
    }

    async fn terminate(&self, extra_records: Option<Vec<FacetRuntime>>) -> Vec<pi_js::Error> {
        todo!("port: FacetKernel::terminate")
    }

    async fn dispose_lifecycles(&self) -> Vec<pi_js::Error> {
        todo!("port: FacetKernel::dispose_lifecycles")
    }
}

async fn dispose_facet_records(records: Vec<FacetRuntime>) -> Vec<pi_js::Error> {
    todo!("port: dispose_facet_records")
}

fn validate_facets(
    records: &[FacetShape],
    external_services: &IndexMap<String, ExternalService>,
) -> pi_js::Result<Vec<String>> {
    todo!("port: validate_facets")
}

fn topological_order(
    records: &[FacetShape],
    dependencies: &IndexMap<String, IndexSet<String>>,
    dependents: &IndexMap<String, IndexSet<String>>,
) -> pi_js::Result<Vec<String>> {
    todo!("port: topological_order")
}

fn record_service_reference(target: &mut Vec<FacetServiceReference>, service: &Service, mode: ServiceMode) {
    todo!("port: record_service_reference")
}

fn same_facet_shape(left: &FacetShape, right: &FacetShape) -> bool {
    todo!("port: same_facet_shape")
}

fn same_references(left: &[FacetServiceReference], right: &[FacetServiceReference]) -> bool {
    todo!("port: same_references")
}

fn is_promise_like(value: &crate::services::handle::ServiceValue) -> bool {
    todo!("port: is_promise_like")
}
