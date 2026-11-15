//! Port of packages/chord/src/delta/tracker.ts

pub use crate::delta::Op;
pub use crate::delta::draft::Draft;
pub use crate::types::JsonValue;

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use indexmap::{IndexMap, IndexSet};

use crate::delta::apply_immutable_trusted::apply_immutable_trusted;
use crate::delta::{
    AppendVerb, DeleteVerb, MoveVerb, NonEmptyPath, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice, OpTruncate,
    Path, RESERVED_SEGMENTS, ReplaceVerb, Seg, SetVerb, SpliceVerb, TruncateVerb, apply_immutable, diff_revisions,
    overlap,
};
use pi_js::Result;

const MAX_DELTA_OPERATIONS: usize = 4_096;
const MAX_SIMPLE_OBJECT_NODES: usize = 128;
const MAX_RETAINED_NODES: usize = 4_096;
const INITIAL_PRUNE_BUDGET: i64 = 256;
const PIECE_PRIORITY_SEED: u32 = 0x9e3779b9;
const STRING_OVERLAP_SCAN: i64 = 65_536;
const DENSE_THRESHOLD: usize = 256;
const MAX_ARRAY_INDEX: i64 = 4_294_967_295;
const MAX_ARRAY_LENGTH: i64 = 4_294_967_296;

static RELEASED: LazyLock<Container> = LazyLock::new(|| Container {
    inner: Arc::new(ContainerInner {
        state: Mutex::new(ContainerState { value: None }),
    }),
});

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Open,
    Prepared,
    Consumed,
    Aborted,
    Stale,
}

// PORT: `{ value }` is shared with `Change` after the context is cleared, so the cell is its own handle.
#[derive(Clone)]
struct StatusCell {
    inner: Arc<Mutex<Status>>,
}

impl StatusCell {
    fn new(status: Status) -> Self {
        Self {
            inner: Arc::new(Mutex::new(status)),
        }
    }

    fn get(&self) -> Status {
        *lock(&self.inner)
    }

    fn set(&self, status: Status) {
        *lock(&self.inner) = status;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParentKind {
    /// Object property.
    Object = 0,
    /// Base-array entry.
    BaseArrayEntry = 1,
    /// Inserted-array entry.
    InsertedArrayEntry = 2,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ParentKey {
    String(String),
    Index(i64),
}

enum PropertyKey {
    String(String),
}

/// Property descriptor returned by [`DraftOverlay::get_descriptor`].
#[derive(Clone, Debug, PartialEq)]
pub struct DraftPropertyDescriptor {
    pub configurable: bool,
    pub enumerable: bool,
    pub writable: bool,
    pub value: DraftValue,
}

/// Value returned from reading a [`DraftOverlay`].
#[derive(Clone, Debug, PartialEq)]
pub enum DraftValue {
    Undefined,
    Json(JsonValue),
    Overlay(DraftOverlay),
}

impl DraftValue {
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }

    pub fn as_json(&self) -> Option<&JsonValue> {
        match self {
            Self::Json(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_overlay(&self) -> Option<&DraftOverlay> {
        match self {
            Self::Overlay(o) => Some(o),
            _ => None,
        }
    }

    /// Snapshot as `Option<JsonValue>` (`None` for [`DraftValue::Undefined`]).
    pub fn to_json(&self) -> Result<Option<JsonValue>> {
        match self {
            Self::Undefined => Ok(None),
            Self::Json(v) => Ok(Some(v.clone())),
            Self::Overlay(o) => o.to_json().map(Some),
        }
    }
}

/// Value passed when mutating a [`DraftOverlay`].
#[derive(Clone, Debug)]
pub enum DraftInput {
    Undefined,
    Json(JsonValue),
    Overlay(DraftOverlay),
    /// Non-finite float (`NaN` / `±Infinity`) rejected as non-strict JSON on write.
    NonFiniteFloat,
}

impl From<JsonValue> for DraftInput {
    fn from(value: JsonValue) -> Self {
        Self::Json(value)
    }
}

impl From<DraftOverlay> for DraftInput {
    fn from(overlay: DraftOverlay) -> Self {
        Self::Overlay(overlay)
    }
}

impl From<DraftValue> for DraftInput {
    fn from(value: DraftValue) -> Self {
        match value {
            DraftValue::Undefined => Self::Undefined,
            DraftValue::Json(v) => Self::Json(v),
            DraftValue::Overlay(o) => Self::Overlay(o),
        }
    }
}

impl From<bool> for DraftInput {
    fn from(value: bool) -> Self {
        Self::Json(JsonValue::Bool(value))
    }
}

impl From<i64> for DraftInput {
    fn from(value: i64) -> Self {
        Self::Json(JsonValue::from(value))
    }
}

impl From<f64> for DraftInput {
    fn from(value: f64) -> Self {
        match serde_json::Number::from_f64(value) {
            Some(n) => Self::Json(JsonValue::Number(n)),
            // Non-finite floats are not strict JSON and must error rather than deleting object properties.
            None => Self::NonFiniteFloat,
        }
    }
}

impl From<&str> for DraftInput {
    fn from(value: &str) -> Self {
        Self::Json(JsonValue::String(value.to_owned()))
    }
}

impl From<String> for DraftInput {
    fn from(value: String) -> Self {
        Self::Json(JsonValue::String(value))
    }
}

#[derive(Clone)]
enum Placement {
    Undefined,
    Json(JsonValue),
    Node(OverlayNode),
    NonFiniteFloat,
}

impl From<DraftInput> for Placement {
    fn from(value: DraftInput) -> Self {
        match value {
            DraftInput::Undefined => Self::Undefined,
            DraftInput::Json(v) => Self::Json(v),
            DraftInput::Overlay(o) => Self::Node(o.node),
            DraftInput::NonFiniteFloat => Self::NonFiniteFloat,
        }
    }
}

enum GetValue {
    Absent,
    Json(JsonValue),
    Node(OverlayNode),
}

impl From<GetValue> for DraftValue {
    fn from(value: GetValue) -> Self {
        match value {
            GetValue::Absent => Self::Undefined,
            GetValue::Json(v) => Self::Json(v),
            GetValue::Node(node) => Self::Overlay(DraftOverlay { node }),
        }
    }
}

#[derive(Clone)]
struct Container {
    inner: Arc<ContainerInner>,
}

struct ContainerInner {
    state: Mutex<ContainerState>,
}

struct ContainerState {
    /// `None` is the shared [`RELEASED`] sentinel (`{}` in TS).
    value: Option<JsonValue>,
}

impl Container {
    fn from_json(value: JsonValue) -> Self {
        Self {
            inner: Arc::new(ContainerInner {
                state: Mutex::new(ContainerState { value: Some(value) }),
            }),
        }
    }

    fn is_array(&self) -> bool {
        let guard = lock(&self.inner.state);
        matches!(guard.value, Some(JsonValue::Array(_)))
    }

    fn array_len(&self) -> usize {
        let guard = lock(&self.inner.state);
        match &guard.value {
            Some(JsonValue::Array(arr)) => arr.len(),
            _ => 0,
        }
    }

    fn to_json(&self) -> Result<JsonValue> {
        let guard = lock(&self.inner.state);
        guard
            .value
            .clone()
            .ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))
    }
}

impl PartialEq for Container {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Container {}

impl Hash for Container {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state);
    }
}

#[derive(Clone)]
enum Stored {
    Json(JsonValue),
    Container(Container),
}

impl Stored {
    fn is_container(&self) -> bool {
        matches!(self, Self::Container(_))
    }

    fn into_json(self) -> Result<JsonValue> {
        match self {
            Self::Json(v) => Ok(v),
            Self::Container(c) => c.to_json(),
        }
    }
}

#[derive(Clone)]
enum StoredRef {
    Json(JsonValue),
    Index { index: i64 },
}

impl StoredRef {
    fn is_index(&self) -> Option<usize> {
        match self {
            Self::Index { index } => Some(*index as usize),
            Self::Json(_) => None,
        }
    }
}

#[derive(Clone)]
struct InsertSource {
    inner: Arc<InsertSourceInner>,
}

struct InsertSourceInner {
    refs: Mutex<Vec<StoredRef>>,
}

impl InsertSource {
    fn new(refs: Vec<StoredRef>) -> Self {
        Self {
            inner: Arc::new(InsertSourceInner { refs: Mutex::new(refs) }),
        }
    }

    fn len(&self) -> i64 {
        lock(&self.inner.refs).len() as i64
    }

    fn get(&self, index: usize) -> StoredRef {
        lock(&self.inner.refs)[index].clone()
    }

    fn extend_from(&self, other: &InsertSource, start: i64, length: i64) {
        let other_refs = lock(&other.inner.refs);
        let slice = &other_refs[start as usize..(start + length) as usize];
        let mut self_refs = lock(&self.inner.refs);
        self_refs.extend_from_slice(slice);
    }
}

impl PartialEq for InsertSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for InsertSource {}

impl Hash for InsertSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state);
    }
}

#[derive(Clone)]
struct Piece {
    inner: Arc<Mutex<PieceData>>,
}

impl Piece {
    fn base(start: i64, length: i64, step: i64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PieceData::Base { start, length, step })),
        }
    }

    fn insert(source: InsertSource, start: i64, length: i64, step: i64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PieceData::Insert {
                source,
                start,
                length,
                step,
            })),
        }
    }

    fn data(&self) -> PieceData {
        lock(&self.inner).clone()
    }

    fn len(&self) -> i64 {
        match &*lock(&self.inner) {
            PieceData::Base { length, .. } | PieceData::Insert { length, .. } => *length,
        }
    }
}

#[derive(Clone)]
enum PieceData {
    Base {
        start: i64,
        length: i64,
        step: i64,
    },
    Insert {
        source: InsertSource,
        start: i64,
        length: i64,
        step: i64,
    },
}

impl PieceData {
    fn length(&self) -> i64 {
        match self {
            Self::Base { length, .. } | Self::Insert { length, .. } => *length,
        }
    }

    fn start(&self) -> i64 {
        match self {
            Self::Base { start, .. } | Self::Insert { start, .. } => *start,
        }
    }

    fn step(&self) -> i64 {
        match self {
            Self::Base { step, .. } | Self::Insert { step, .. } => *step,
        }
    }

    fn is_base(&self) -> bool {
        matches!(self, Self::Base { .. })
    }

    fn source(&self) -> Option<&InsertSource> {
        match self {
            Self::Base { .. } => None,
            Self::Insert { source, .. } => Some(source),
        }
    }

    fn add_length(&mut self, amount: i64) {
        match self {
            Self::Base { length, .. } | Self::Insert { length, .. } => *length += amount,
        }
    }

    fn set_step(&mut self, next: i64) {
        match self {
            Self::Base { step, .. } | Self::Insert { step, .. } => *step = next,
        }
    }

    fn reverse_in_place(&mut self) {
        match self {
            Self::Base { start, length, step }
            | Self::Insert {
                start, length, step, ..
            } => {
                *start += *step * (*length - 1);
                *step = if *step == 1 { -1 } else { 1 };
            }
        }
    }
}

struct PieceNode {
    piece: Piece,
    left: Option<Box<PieceNode>>,
    right: Option<Box<PieceNode>>,
    priority: u32,
    elements: i64,
}

#[derive(Clone)]
struct PieceLocation {
    piece: Piece,
    logical_start: i64,
    minimum: i64,
    maximum: i64,
}

struct DenseRegion {
    start: i64,
    length: i64,
}

struct DenseCandidates {
    indices: Vec<i64>,
    bits: Option<Vec<u8>>,
    length: usize,
}

#[derive(Clone)]
struct ArrayPlan {
    remove_runs: Vec<i64>,
    permutation: Option<Vec<i64>>,
    insert_runs: Vec<i64>,
}

#[derive(Clone)]
struct ArrayOverlay {
    inner: Arc<Mutex<ArrayOverlayState>>,
}

struct ArrayOverlayState {
    root: Option<Box<PieceNode>>,
    pieces: Option<Vec<Piece>>,
    base_overrides: Option<Arc<Mutex<IndexMap<i64, StoredRef>>>>,
    insert_overrides: Option<Arc<Mutex<IndexMap<InsertSource, Arc<Mutex<IndexMap<i64, StoredRef>>>>>>>,
    structural: bool,
    generation: i64,
    plan: Option<ArrayPlan>,
    seed: u32,
    located_offset: i64,
    base_locations: Option<Vec<PieceLocation>>,
    insert_locations: Option<IndexMap<InsertSource, Vec<PieceLocation>>>,
}

#[derive(Clone)]
struct OverlayNode {
    inner: Arc<OverlayNodeInner>,
}

struct OverlayNodeInner {
    context: OverlayContext,
    base_index: usize,
    parent_index: Option<usize>,
    parent_kind: ParentKind,
    parent_key: ParentKey,
    parent_source: Option<InsertSource>,
    parent_placement: bool,
    state: Mutex<OverlayNodeState>,
}

struct OverlayNodeState {
    target: Container,
    write_key: Option<String>,
    write_value: Option<StoredRef>,
    writes: Option<IndexMap<String, StoredRef>>,
    delete_key: Option<String>,
    deletes: Option<IndexSet<String>>,
    readded: Option<IndexSet<String>>,
    base_object_children: IndexMap<String, Container>,
    base_array_children: IndexMap<i64, Container>,
    array: Option<ArrayOverlay>,
    dirty: bool,
    subtree_dirty: bool,
    prepared_path: Option<Path>,
}

impl PartialEq for OverlayNode {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for OverlayNode {}

impl Hash for OverlayNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state);
    }
}

#[derive(Clone)]
struct ContextRef(Weak<OverlayContextInner>);

impl PartialEq for ContextRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl Eq for ContextRef {}

impl Hash for ContextRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_ptr().hash(state);
    }
}

#[derive(Clone)]
struct OverlayContext {
    inner: Arc<OverlayContextInner>,
}

struct OverlayContextInner {
    owner: Arc<()>,
    tracker: Weak<dyn TrackerControl>,
    base_revision: i64,
    replacement: bool,
    status: StatusCell,
    state: Mutex<OverlayContextState>,
}

struct OverlayContextState {
    root: Option<OverlayNode>,
    bases: Vec<Container>,
    stored: Vec<Container>,
    dirty: Vec<OverlayNode>,
    nodes: Vec<OverlayNode>,
    raw_nodes: IndexMap<Container, OverlayNode>,
    ops: Option<Vec<Op>>,
    replacement_noop: bool,
    base_value: Option<JsonValue>,
    overlay_released: bool,
    simple_object_materialization: bool,
    registry_ref: Option<ContextRef>,
}

trait TrackerControl: Send + Sync {
    fn release_context(&self, context: &OverlayContext);
    fn replacement_base_json(&self, context: &OverlayContext) -> Option<JsonValue>;
}

struct TrackerState<T> {
    contexts: IndexSet<ContextRef>,
    self_ref: Weak<TrackerInner<T>>,
    value: T,
    revision: i64,
    prune_budget: i64,
}

struct TrackerInner<T> {
    owner: Arc<()>,
    state: Mutex<TrackerState<T>>,
}

impl<T: Send + Sync + 'static> TrackerControl for TrackerInner<T> {
    fn release_context(&self, context: &OverlayContext) {
        let reg = {
            let mut ctx_guard = lock(&context.inner.state);
            ctx_guard.registry_ref.take()
        };
        if let Some(reference) = reg {
            let mut guard = lock(&self.state);
            guard.contexts.shift_remove(&reference);
        }
    }

    fn replacement_base_json(&self, context: &OverlayContext) -> Option<JsonValue> {
        let guard = lock(&self.state);
        if context.inner.base_revision != guard.revision {
            return None;
        }
        crate::json::clone_as_json(&guard.value).ok()
    }
}

struct PreparedInner<T> {
    context: OverlayContext,
    value: T,
    base: T,
    ops: Vec<Op>,
}

struct ChangeState {
    context: Option<OverlayContext>,
    prepared_status: Option<StatusCell>,
    settled: bool,
}

struct ChangeInner<T> {
    state: Mutex<ChangeState>,
    _marker: std::marker::PhantomData<T>,
}

/// Prepared immutable candidate revision and its delta operation batch.
#[derive(Clone)]
pub struct Prepared<T> {
    inner: Arc<PreparedInner<T>>,
}

impl<T> Prepared<T> {
    pub fn base(&self) -> &T {
        &self.inner.base
    }

    pub fn value(&self) -> &T {
        &self.inner.value
    }

    pub fn ops(&self) -> &[Op] {
        &self.inner.ops
    }

    pub fn base_revision(&self) -> i64 {
        self.inner.context.inner.base_revision
    }

    pub fn abort(&self) {
        abort_context(&self.inner.context);
    }
}

/// Public handle to an active overlay draft node, mirroring TS Proxy semantics in Rust.
#[derive(Clone)]
pub struct DraftOverlay {
    node: OverlayNode,
}

impl PartialEq for DraftOverlay {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl Eq for DraftOverlay {}

impl std::fmt::Debug for DraftOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftOverlay")
            .field("base_index", &self.node.inner.base_index)
            .finish()
    }
}

impl DraftOverlay {
    pub fn is_array(&self) -> Result<bool> {
        assert_readable(&self.node.inner.context)?;
        Ok(node_base(&self.node).is_array())
    }

    pub fn len(&self) -> Result<usize> {
        assert_readable(&self.node.inner.context)?;
        if node_base(&self.node).is_array() {
            Ok(array_length(&array_overlay(&self.node)) as usize)
        } else {
            Ok(own_keys(&self.node)?.len())
        }
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn get(&self, property: &str) -> Result<DraftValue> {
        if property == "then" && is_settled_context(&self.node.inner.context) {
            return Ok(DraftValue::Undefined);
        }
        Ok(get_property(&self.node, PropertyKey::String(property.to_owned()))?.into())
    }

    pub fn get_index(&self, index: i64) -> Result<DraftValue> {
        assert_readable(&self.node.inner.context)?;
        if node_base(&self.node).is_array() {
            if index < 0 || index >= MAX_ARRAY_INDEX {
                return get_property(&self.node, PropertyKey::String(index.to_string())).map(Into::into);
            }
            return get_array_index(&self.node, index).map(Into::into);
        }
        get_property(&self.node, PropertyKey::String(index.to_string())).map(Into::into)
    }

    pub fn set(&self, property: &str, value: impl Into<DraftInput>) -> Result<bool> {
        set_property(
            &self.node,
            PropertyKey::String(property.to_owned()),
            Placement::from(value.into()),
        )
    }

    pub fn set_index(&self, index: i64, value: impl Into<DraftInput>) -> Result<bool> {
        if index < 0 || index >= MAX_ARRAY_INDEX {
            return Err(pi_js::Error::js(
                "TypeError",
                "Only array indices and length can be written",
            ));
        }
        set_property(
            &self.node,
            PropertyKey::String(index.to_string()),
            Placement::from(value.into()),
        )
    }

    pub fn delete(&self, property: &str) -> Result<bool> {
        delete_property(&self.node, PropertyKey::String(property.to_owned()))
    }

    pub fn delete_index(&self, index: i64) -> Result<bool> {
        delete_property(&self.node, PropertyKey::String(index.to_string()))
    }

    pub fn has(&self, property: &str) -> Result<bool> {
        has_property(&self.node, PropertyKey::String(property.to_owned()))
    }

    pub fn own_keys(&self) -> Result<Vec<String>> {
        own_keys(&self.node)
    }

    pub fn get_descriptor(&self, property: &str) -> Result<Option<DraftPropertyDescriptor>> {
        get_descriptor(&self.node, PropertyKey::String(property.to_owned()))
    }

    pub fn set_len(&self, next: i64) -> Result<()> {
        assert_writable(&self.node.inner.context)?;
        if !node_base(&self.node).is_array() {
            let _ = set_property(
                &self.node,
                PropertyKey::String("length".to_owned()),
                Placement::Json(JsonValue::from(next)),
            )?;
            return Ok(());
        }
        if next < 0 || next >= MAX_ARRAY_LENGTH {
            return Err(pi_js::Error::js("RangeError", "Invalid array length"));
        }
        set_array_length(&self.node, next);
        Ok(())
    }

    pub fn push(&self, items: &[DraftInput]) -> Result<i64> {
        let mut placements: Vec<Placement> = items.iter().cloned().map(Placement::from).collect();
        array_push(&self.node, &mut placements)
    }

    pub fn pop(&self) -> Result<DraftValue> {
        array_pop(&self.node).map(Into::into)
    }

    pub fn shift(&self) -> Result<DraftValue> {
        array_shift(&self.node).map(Into::into)
    }

    pub fn unshift(&self, items: &[DraftInput]) -> Result<i64> {
        let mut placements: Vec<Placement> = items.iter().cloned().map(Placement::from).collect();
        array_unshift(&self.node, &mut placements)
    }

    pub fn splice(
        &self,
        start: Option<i64>,
        delete_count: Option<i64>,
        items: &[DraftInput],
    ) -> Result<Vec<DraftValue>> {
        let mut placements: Vec<Placement> = items.iter().cloned().map(Placement::from).collect();
        let removed = array_splice(&self.node, start, delete_count, &mut placements)?;
        Ok(removed.into_iter().map(Into::into).collect())
    }

    pub fn reverse(&self) -> Result<DraftOverlay> {
        array_reverse(&self.node)
    }

    pub fn sort(&self) -> Result<DraftOverlay> {
        array_sort(&self.node, None::<&mut fn(DraftValue, DraftValue) -> Result<Ordering>>)
    }

    pub fn sort_by<F>(&self, mut comparator: F) -> Result<DraftOverlay>
    where
        F: FnMut(DraftValue, DraftValue) -> Result<Ordering>,
    {
        array_sort(&self.node, Some(&mut comparator))
    }

    pub fn fill(&self, value: impl Into<DraftInput>, start: Option<i64>, end: Option<i64>) -> Result<DraftOverlay> {
        array_fill(&self.node, &Placement::from(value.into()), start, end)
    }

    pub fn copy_within(&self, target: i64, start: i64, end: Option<i64>) -> Result<DraftOverlay> {
        array_copy_within(&self.node, target, start, end)
    }

    pub fn to_json(&self) -> Result<JsonValue> {
        assert_readable(&self.node.inner.context)?;
        clone_node(&self.node)?.to_json()
    }

    pub fn define_property(&self) -> Result<()> {
        assert_writable(&self.node.inner.context)?;
        Err(pi_js::Error::js(
            "TypeError",
            "Defining overlay properties is not supported",
        ))
    }

    pub fn prevent_extensions(&self) -> Result<()> {
        assert_writable(&self.node.inner.context)?;
        Err(pi_js::Error::js("TypeError", "Overlays cannot be made non-extensible"))
    }

    pub fn set_prototype(&self) -> Result<()> {
        assert_writable(&self.node.inner.context)?;
        Err(pi_js::Error::js(
            "TypeError",
            "Changing an overlay prototype is not supported",
        ))
    }
}

/// Active transactional overlay change handle.
#[derive(Clone)]
pub struct Change<T> {
    inner: Arc<ChangeInner<T>>,
}

impl<T: Send + Sync + 'static> Change<T> {
    /// Returns a cloned snapshot `Draft<T>` while the change is active.
    pub fn state(&self) -> Result<Draft<T>> {
        self.overlay()?.to_json().and_then(crate::json::from_json)
    }

    /// Returns the live proxy-semantic [`DraftOverlay`] handle for this active change.
    pub fn overlay(&self) -> Result<DraftOverlay> {
        let context = lock(&self.inner.state).context.clone();
        let Some(context) = context else {
            return Err(pi_js::Error::js("TypeError", "Cannot use a settled overlay"));
        };
        assert_readable(&context)?;
        let root = lock(&context.inner.state).root.clone();
        let Some(root) = root else {
            return Err(pi_js::Error::js("TypeError", "Cannot use a settled overlay"));
        };
        Ok(DraftOverlay { node: root })
    }

    pub fn prepare(&self) -> Result<Prepared<T>> {
        let context = {
            let guard = lock(&self.inner.state);
            if guard.settled {
                return Err(pi_js::Error::msg("Change has already been settled"));
            }
            guard.context.clone()
        };
        let Some(context) = context else {
            return Err(pi_js::Error::js("TypeError", "Cannot use a settled overlay"));
        };
        assert_writable(&context)?;
        context.inner.status.set(Status::Prepared);
        let result = (|| -> Result<Prepared<T>> {
            if !context.inner.replacement {
                let is_empty_dirty = lock(&context.inner.state).dirty.is_empty();
                let ops = if is_empty_dirty {
                    Vec::new()
                } else {
                    emit_operations(&context)?
                };
                lock(&context.inner.state).ops = Some(ops);
            }
            materialize_prepared::<T>(&context)
        })();

        match result {
            Ok(prepared) => {
                let mut guard = lock(&self.inner.state);
                guard.prepared_status = Some(context.inner.status.clone());
                guard.settled = true;
                guard.context = None;
                Ok(prepared)
            }
            Err(error) => {
                context.inner.status.set(Status::Aborted);
                clear_context(&context);
                let mut guard = lock(&self.inner.state);
                guard.settled = true;
                guard.context = None;
                Err(error)
            }
        }
    }

    /// Commits direct mutations performed on a `Draft<T>` (`T`) snapshot against this change's base.
    pub fn prepare_with(&self, next: T) -> Result<Prepared<T>> {
        let context = {
            let guard = lock(&self.inner.state);
            if guard.settled {
                return Err(pi_js::Error::msg("Change has already been settled"));
            }
            guard.context.clone()
        };
        let Some(context) = context else {
            return Err(pi_js::Error::js("TypeError", "Cannot use a settled overlay"));
        };
        assert_writable(&context)?;
        context.inner.status.set(Status::Prepared);
        let result = (|| -> Result<Prepared<T>> {
            let next_json = crate::json::take_as_json(next)?;
            if !crate::json::is_json_value(&next_json) {
                return Err(pi_js::Error::js("TypeError", "Value must be strict JSON"));
            }
            let base_json = lock(&context.inner.state)
                .base_value
                .clone()
                .ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))?;
            let ops = if equal_trusted_json(&base_json, &next_json) {
                Vec::new()
            } else {
                diff_revisions(&base_json, &next_json)
            };
            let value = if ops.is_empty() {
                crate::json::from_json::<T>(base_json.clone())?
            } else {
                crate::json::from_json::<T>(next_json)?
            };
            let base = crate::json::from_json::<T>(base_json)?;
            lock(&context.inner.state).ops = Some(ops.clone());
            let prepared = Prepared {
                inner: Arc::new(PreparedInner {
                    context: context.clone(),
                    value,
                    base,
                    ops,
                }),
            };
            clear_context(&context);
            Ok(prepared)
        })();

        match result {
            Ok(prepared) => {
                let mut guard = lock(&self.inner.state);
                guard.prepared_status = Some(context.inner.status.clone());
                guard.settled = true;
                guard.context = None;
                Ok(prepared)
            }
            Err(error) => {
                context.inner.status.set(Status::Aborted);
                clear_context(&context);
                let mut guard = lock(&self.inner.state);
                guard.settled = true;
                guard.context = None;
                Err(error)
            }
        }
    }

    pub fn abort(&self) {
        let context = {
            let mut guard = lock(&self.inner.state);
            if guard.settled {
                if let Some(status) = guard.prepared_status.take() {
                    if status.get() == Status::Prepared {
                        status.set(Status::Aborted);
                    }
                }
                return;
            }
            guard.settled = true;
            guard.context.take()
        };
        if let Some(context) = context {
            abort_context(&context);
        }
    }
}

/// Immutable revision tracker with copy-on-write transactional overlay changes.
#[derive(Clone)]
pub struct Tracker<T> {
    inner: Arc<TrackerInner<T>>,
}

impl<T: Send + Sync + 'static> Tracker<T> {
    // PORT: the root is swapped under a mutex, so this clones instead of returning a JS reference.
    pub fn value(&self) -> T
    where
        T: Clone,
    {
        lock(&self.inner.state).value.clone()
    }

    pub fn revision(&self) -> i64 {
        lock(&self.inner.state).revision
    }

    pub fn begin_change(&self) -> Change<T> {
        let (self_ref, revision, base_json) = {
            let guard = lock(&self.inner.state);
            let json = crate::json::clone_as_json(&guard.value).unwrap_or(JsonValue::Null);
            (guard.self_ref.clone(), guard.revision, json)
        };
        let root = Container::from_json(base_json.clone());
        let context = create_context(
            self_ref,
            Arc::clone(&self.inner.owner),
            revision,
            root,
            false,
            base_json,
        );
        self.register(&context);
        Change {
            inner: Arc::new(ChangeInner {
                state: Mutex::new(ChangeState {
                    context: Some(context),
                    prepared_status: None,
                    settled: false,
                }),
                _marker: std::marker::PhantomData,
            }),
        }
    }

    pub fn prepare_replace(&self, value: T) -> Result<Prepared<T>> {
        let value_json = crate::json::take_as_json(value)?;
        let (self_ref, revision, base_json) = {
            let guard = lock(&self.inner.state);
            let base = crate::json::clone_as_json(&guard.value)?;
            (guard.self_ref.clone(), guard.revision, base)
        };
        let root = Container::from_json(value_json);
        let context = create_context(self_ref, Arc::clone(&self.inner.owner), revision, root, true, base_json);
        context.inner.status.set(Status::Prepared);
        self.register(&context);
        match materialize_prepared::<T>(&context) {
            Ok(prepared) => Ok(prepared),
            Err(error) => {
                context.inner.status.set(Status::Aborted);
                clear_context(&context);
                Err(error)
            }
        }
    }

    pub fn adopt(&self, prepared: &Prepared<T>) -> Result<()> {
        let context = &prepared.inner.context;
        if !Arc::ptr_eq(&context.inner.owner, &self.inner.owner) {
            return Err(pi_js::Error::msg("Prepared change belongs to a different tracker"));
        }
        match context.inner.status.get() {
            Status::Consumed => {
                return Err(pi_js::Error::msg("Prepared change has already been used"));
            }
            Status::Aborted => {
                return Err(pi_js::Error::msg("Prepared change has been aborted"));
            }
            Status::Stale => {
                return Err(pi_js::Error::msg("Prepared change is stale"));
            }
            Status::Open => {
                return Err(pi_js::Error::msg("Prepared change is not ready"));
            }
            Status::Prepared => {}
        }

        let stale_revision = {
            let guard = lock(&self.inner.state);
            context.inner.base_revision != guard.revision
        };
        if stale_revision {
            context.inner.status.set(Status::Stale);
            clear_context(context);
            return Err(pi_js::Error::msg("Prepared change is stale"));
        }

        let next_value = {
            let next_json = crate::json::clone_as_json(&prepared.inner.value)?;
            crate::json::from_json::<T>(next_json)?
        };
        {
            let mut guard = lock(&self.inner.state);
            if context.inner.base_revision != guard.revision {
                context.inner.status.set(Status::Stale);
                drop(guard);
                clear_context(context);
                return Err(pi_js::Error::msg("Prepared change is stale"));
            }
            guard.value = next_value;
            context.inner.status.set(Status::Consumed);
            guard.revision += 1;
        }
        self.invalidate(context);
        Ok(())
    }

    fn register(&self, context: &OverlayContext) {
        let reference = ContextRef(Arc::downgrade(&context.inner));
        lock(&context.inner.state).registry_ref = Some(reference.clone());
        let mut guard = lock(&self.inner.state);
        guard.contexts.insert(reference);
        guard.prune_budget -= 1;
        if guard.prune_budget == 0 {
            guard.contexts.retain(|r| r.0.strong_count() > 0);
            guard.prune_budget = INITIAL_PRUNE_BUDGET.max(guard.contexts.len() as i64);
        }
    }

    // Adoption must not walk every node touched by a competing draft. Drop the
    // context's strong overlay references in O(1); externally retained proxies
    // still see the stale status and are reclaimed with their holders.
    fn invalidate(&self, winner: &OverlayContext) {
        let contexts = {
            let mut guard = lock(&self.inner.state);
            let taken = std::mem::take(&mut guard.contexts);
            guard.prune_budget = INITIAL_PRUNE_BUDGET;
            taken
        };
        for reference in contexts {
            let Some(arc) = reference.0.upgrade() else {
                continue;
            };
            let context = OverlayContext { inner: arc };
            if Arc::ptr_eq(&context.inner, &winner.inner) {
                continue;
            }
            let current = context.inner.status.get();
            if current == Status::Open || current == Status::Prepared {
                context.inner.status.set(Status::Stale);
            }
            release_overlay_references_only(&context);
        }
    }
}

/// Take immutable ownership of an alias-free strict-JSON root in O(1).
pub fn track<T: Send + Sync + 'static>(initial: T) -> Tracker<T> {
    let inner = Arc::new_cyclic(|weak| TrackerInner {
        owner: Arc::new(()),
        state: Mutex::new(TrackerState {
            contexts: IndexSet::new(),
            self_ref: weak.clone(),
            value: initial,
            revision: 0,
            prune_budget: INITIAL_PRUNE_BUDGET,
        }),
    });
    Tracker { inner }
}

fn materialize_prepared<T: 'static>(context: &OverlayContext) -> Result<Prepared<T>> {
    let operations = ensure_operations(context)?;
    let (base_json, simple_mat, root_opt) = {
        let guard = lock(&context.inner.state);
        (
            guard.base_value.clone(),
            guard.simple_object_materialization,
            guard.root.clone(),
        )
    };
    let base_json = base_json.ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))?;
    let base = crate::json::from_json::<T>(base_json.clone())?;
    // Operation payloads are already detached from the mutable overlay. The
    // resulting revision and public batch intentionally share those payloads under
    // the trusted immutable-transfer contract.
    let value = if operations.is_empty() {
        crate::json::from_json::<T>(base_json)?
    } else if context.inner.replacement {
        match &operations[0] {
            Op::Replace(OpReplace(_, next)) => crate::json::from_json::<T>(next.clone())?,
            _ => materialize_operations(base_json, &operations)?,
        }
    } else if simple_mat {
        let root = root_opt.ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))?;
        let container = clone_node(&root)?;
        crate::json::from_json::<T>(container.to_json()?)?
    } else {
        materialize_operations(base_json, &operations)?
    };
    let prepared = Prepared {
        inner: Arc::new(PreparedInner {
            context: context.clone(),
            value,
            base,
            ops: operations,
        }),
    };
    let node_count = lock(&context.inner.state).nodes.len();
    if node_count > MAX_RETAINED_NODES {
        release_overlay_references(context);
    } else {
        clear_context(context);
    }
    Ok(prepared)
}

fn materialize_operations<T: 'static>(base_json: JsonValue, operations: &[Op]) -> Result<T> {
    for operation in operations {
        // Native splice/permutation remains faster for large structural batches.
        if matches!(operation, Op::Splice(_) | Op::Move(_)) {
            let applied = apply_immutable(Some(base_json), operations)?;
            return crate::json::from_json(applied);
        }
    }
    let applied = apply_immutable_trusted(base_json, operations)?;
    crate::json::from_json(applied)
}

fn create_context(
    tracker: Weak<dyn TrackerControl>,
    owner: Arc<()>,
    base_revision: i64,
    root: Container,
    replacement: bool,
    base: JsonValue,
) -> OverlayContext {
    let context = OverlayContext {
        inner: Arc::new(OverlayContextInner {
            owner,
            tracker,
            base_revision,
            replacement,
            status: StatusCell::new(Status::Open),
            state: Mutex::new(OverlayContextState {
                root: None,
                bases: Vec::new(),
                stored: Vec::new(),
                dirty: Vec::new(),
                nodes: Vec::new(),
                raw_nodes: IndexMap::new(),
                ops: None,
                replacement_noop: false,
                base_value: Some(base),
                overlay_released: false,
                simple_object_materialization: false,
                registry_ref: None,
            }),
        }),
    };
    let root_node = create_node(
        &context,
        root,
        None,
        ParentKind::Object,
        ParentKey::String(String::new()),
        None,
        false,
    );
    lock(&context.inner.state).root = Some(root_node);
    context
}

fn create_node(
    context: &OverlayContext,
    base: Container,
    parent: Option<&OverlayNode>,
    parent_kind: ParentKind,
    parent_key: ParentKey,
    parent_source: Option<InsertSource>,
    parent_placement: bool,
) -> OverlayNode {
    if let Some(existing) = lock(&context.inner.state).raw_nodes.get(&base).cloned() {
        return existing;
    }
    let is_arr = base.is_array();
    let target = Container::from_json(if is_arr {
        JsonValue::Array(Vec::new())
    } else {
        JsonValue::Object(serde_json::Map::new())
    });
    let base_index = {
        let mut guard = lock(&context.inner.state);
        let idx = guard.bases.len();
        guard.bases.push(base.clone());
        idx
    };
    let parent_index = parent.map(|p| p.inner.base_index);
    let node = OverlayNode {
        inner: Arc::new(OverlayNodeInner {
            context: context.clone(),
            base_index,
            parent_index,
            parent_kind,
            parent_key,
            parent_source,
            parent_placement,
            state: Mutex::new(OverlayNodeState {
                target,
                write_key: None,
                write_value: None,
                writes: None,
                delete_key: None,
                deletes: None,
                readded: None,
                base_object_children: IndexMap::new(),
                base_array_children: IndexMap::new(),
                array: None,
                dirty: false,
                subtree_dirty: false,
                prepared_path: None,
            }),
        }),
    };
    let mut guard = lock(&context.inner.state);
    guard.raw_nodes.insert(base, node.clone());
    guard.nodes.push(node.clone());
    node
}

fn node_base(node: &OverlayNode) -> Container {
    let guard = lock(&node.inner.context.inner.state);
    guard
        .bases
        .get(node.inner.base_index)
        .cloned()
        .unwrap_or_else(|| RELEASED.clone())
}

fn node_base_json(node: &OverlayNode) -> JsonValue {
    let base = node_base(node);
    let guard = lock(&base.inner.state);
    guard.value.clone().unwrap_or(JsonValue::Null)
}

fn node_parent(node: &OverlayNode) -> Option<OverlayNode> {
    let parent_index = node.inner.parent_index?;
    let guard = lock(&node.inner.context.inner.state);
    guard.nodes.get(parent_index).cloned()
}

fn base_child_container(node: &OverlayNode, key: &ParentKey) -> Option<Container> {
    {
        let guard = lock(&node.inner.state);
        match key {
            ParentKey::String(k) => {
                if let Some(existing) = guard.base_object_children.get(k) {
                    return Some(existing.clone());
                }
            }
            ParentKey::Index(i) => {
                if let Some(existing) = guard.base_array_children.get(i) {
                    return Some(existing.clone());
                }
            }
        }
    }
    let base = node_base(node);
    let child_json = {
        let base_guard = lock(&base.inner.state);
        match (&base_guard.value, key) {
            (Some(JsonValue::Object(map)), ParentKey::String(k)) => map.get(k).cloned(),
            (Some(JsonValue::Array(arr)), ParentKey::Index(i)) if *i >= 0 => arr.get(*i as usize).cloned(),
            _ => None,
        }
    }?;
    if !is_container(&child_json) {
        return None;
    }
    let container = Container::from_json(child_json);
    let mut guard = lock(&node.inner.state);
    match key {
        ParentKey::String(k) => {
            if let Some(existing) = guard.base_object_children.get(k) {
                return Some(existing.clone());
            }
            guard.base_object_children.insert(k.clone(), container.clone());
        }
        ParentKey::Index(i) => {
            if let Some(existing) = guard.base_array_children.get(i) {
                return Some(existing.clone());
            }
            guard.base_array_children.insert(*i, container.clone());
        }
    }
    Some(container)
}

fn store_value(context: &OverlayContext, value: Stored) -> StoredRef {
    match value {
        Stored::Json(v) => {
            if !is_container(&v) {
                return StoredRef::Json(v);
            }
            let container = Container::from_json(v);
            let mut guard = lock(&context.inner.state);
            let index = guard.stored.len() as i64;
            guard.stored.push(container);
            StoredRef::Index { index }
        }
        Stored::Container(c) => {
            let mut guard = lock(&context.inner.state);
            let index = guard.stored.len() as i64;
            guard.stored.push(c);
            StoredRef::Index { index }
        }
    }
}

fn stored_value(context: &OverlayContext, reference: &StoredRef) -> Stored {
    match reference {
        StoredRef::Json(v) => Stored::Json(v.clone()),
        StoredRef::Index { index } => {
            let guard = lock(&context.inner.state);
            Stored::Container(guard.stored[*index as usize].clone())
        }
    }
}

fn replace_stored_value(context: &OverlayContext, reference: &StoredRef, value: Stored) -> StoredRef {
    match (reference, value) {
        (StoredRef::Index { index }, Stored::Container(c)) => {
            let mut guard = lock(&context.inner.state);
            guard.stored[*index as usize] = c;
            StoredRef::Index { index: *index }
        }
        (StoredRef::Index { index }, Stored::Json(v)) if is_container(&v) => {
            let container = Container::from_json(v);
            let mut guard = lock(&context.inner.state);
            guard.stored[*index as usize] = container;
            StoredRef::Index { index: *index }
        }
        (StoredRef::Index { index }, Stored::Json(v)) => {
            let mut guard = lock(&context.inner.state);
            guard.stored[*index as usize] = RELEASED.clone();
            StoredRef::Json(v)
        }
        (StoredRef::Json(_), other) => store_value(context, other),
    }
}

fn release_stored_value(context: &OverlayContext, reference: &StoredRef) {
    if let Some(index) = reference.is_index() {
        let mut guard = lock(&context.inner.state);
        if index < guard.stored.len() {
            guard.stored[index] = RELEASED.clone();
        }
    }
}

fn is_settled_context(context: &OverlayContext) -> bool {
    let released = lock(&context.inner.state).overlay_released;
    if released {
        return true;
    }
    matches!(
        context.inner.status.get(),
        Status::Consumed | Status::Aborted | Status::Stale
    )
}

fn assert_readable(context: &OverlayContext) -> Result<()> {
    if is_settled_context(context) {
        return Err(pi_js::Error::js("TypeError", "Cannot use a settled overlay"));
    }
    Ok(())
}

fn assert_writable(context: &OverlayContext) -> Result<()> {
    assert_readable(context)?;
    if context.inner.status.get() != Status::Open {
        return Err(pi_js::Error::js("TypeError", "Prepared overlays are read-only"));
    }
    Ok(())
}

fn get_property(node: &OverlayNode, property: PropertyKey) -> Result<GetValue> {
    assert_readable(&node.inner.context)?;
    if node_base(node).is_array() {
        let overlay = array_overlay(node);
        let PropertyKey::String(prop) = &property;
        if prop == "length" {
            return Ok(GetValue::Json(JsonValue::from(array_length(&overlay))));
        }
        if let Some(index) = array_index(prop) {
            return get_array_index(node, index);
        }
        return Ok(GetValue::Absent);
    }
    let PropertyKey::String(key) = property;
    if !object_has(node, &key) {
        return Ok(GetValue::Absent);
    }
    let Some(value) = object_value(node, &key) else {
        return Ok(GetValue::Absent);
    };
    match value {
        Stored::Json(v) if !is_container(&v) => Ok(GetValue::Json(v)),
        Stored::Json(v) => {
            let c = Container::from_json(v);
            Ok(GetValue::Node(create_node(
                &node.inner.context,
                c,
                Some(node),
                ParentKind::Object,
                ParentKey::String(key.clone()),
                None,
                has_object_write(node, &key),
            )))
        }
        Stored::Container(c) => Ok(GetValue::Node(create_node(
            &node.inner.context,
            c,
            Some(node),
            ParentKind::Object,
            ParentKey::String(key.clone()),
            None,
            has_object_write(node, &key),
        ))),
    }
}

fn get_array_index(node: &OverlayNode, index: i64) -> Result<GetValue> {
    let overlay = array_overlay(node);
    if index >= array_length(&overlay) {
        return Ok(GetValue::Absent);
    }
    let (piece, located_offset) = locate_piece(&overlay, index)?;
    let data = piece.data();
    let source_index = data.start() + data.step() * located_offset;
    let value = entry_value_at(node, &piece, source_index);
    match value {
        Stored::Json(v) if !is_container(&v) => Ok(GetValue::Json(v)),
        Stored::Json(v) => {
            let c = Container::from_json(v);
            Ok(GetValue::Node(create_node(
                &node.inner.context,
                c,
                Some(node),
                if data.is_base() {
                    ParentKind::BaseArrayEntry
                } else {
                    ParentKind::InsertedArrayEntry
                },
                ParentKey::Index(source_index),
                data.source().cloned(),
                !data.is_base() || has_entry_override_at(&overlay, &piece, source_index),
            )))
        }
        Stored::Container(c) => Ok(GetValue::Node(create_node(
            &node.inner.context,
            c,
            Some(node),
            if data.is_base() {
                ParentKind::BaseArrayEntry
            } else {
                ParentKind::InsertedArrayEntry
            },
            ParentKey::Index(source_index),
            data.source().cloned(),
            !data.is_base() || has_entry_override_at(&overlay, &piece, source_index),
        ))),
    }
}

fn set_property(node: &OverlayNode, property: PropertyKey, supplied: Placement) -> Result<bool> {
    assert_writable(&node.inner.context)?;
    let PropertyKey::String(key) = property;
    if node_base(node).is_array() {
        if key == "length" {
            let next_len = match supplied {
                Placement::Json(v) => to_array_length(&v)?,
                Placement::Undefined | Placement::Node(_) | Placement::NonFiniteFloat => {
                    return Err(pi_js::Error::js("RangeError", "Invalid array length"));
                }
            };
            set_array_length(node, next_len);
            return Ok(true);
        }
        let Some(index) = array_index(&key) else {
            return Err(pi_js::Error::js(
                "TypeError",
                "Only array indices and length can be written",
            ));
        };
        if index > array_length(&array_overlay(node)) {
            return Err(pi_js::Error::js("TypeError", "Overlay arrays cannot contain holes"));
        }
        let stored = clone_placement(&supplied)?;
        set_array_index(node, index, stored)?;
        return Ok(true);
    }
    if matches!(supplied, Placement::Undefined) {
        return delete_property(node, PropertyKey::String(key));
    }
    let stored = clone_placement(&supplied)?;
    let current = object_value(node, &key);
    let was_deleted = is_object_deleted(node, &key);
    if !was_deleted && !stored.is_container() {
        if let (Stored::Json(next_prim), Some(Stored::Json(curr_prim))) = (&stored, &current) {
            if !is_container(curr_prim) && equal_primitives(curr_prim, next_prim) {
                return Ok(true);
            }
        }
    }
    set_object_write(node, &key, stored);
    if was_deleted && base_object_has_own(node, &key) {
        let mut guard = lock(&node.inner.state);
        guard.readded.get_or_insert_with(IndexSet::new).insert(key.clone());
    }
    delete_object_deletion(node, &key);
    mark_dirty(node);
    Ok(true)
}

fn delete_property(node: &OverlayNode, property: PropertyKey) -> Result<bool> {
    assert_writable(&node.inner.context)?;
    if node_base(node).is_array() {
        return Err(pi_js::Error::js("TypeError", "Overlay arrays cannot contain holes"));
    }
    let PropertyKey::String(key) = property;
    if !object_has(node, &key) {
        return Ok(true);
    }
    delete_object_write(node, &key);
    {
        let mut guard = lock(&node.inner.state);
        if let Some(readded) = &mut guard.readded {
            readded.shift_remove(&key);
        }
    }
    set_object_deletion(node, &key);
    mark_dirty(node);
    Ok(true)
}

fn has_property(node: &OverlayNode, property: PropertyKey) -> Result<bool> {
    assert_readable(&node.inner.context)?;
    let PropertyKey::String(key) = property;
    if node_base(node).is_array() {
        if key == "length" {
            return Ok(true);
        }
        if let Some(index) = array_index(&key) {
            return Ok(index < array_length(&array_overlay(node)));
        }
        return Ok(false);
    }
    Ok(object_has(node, &key))
}

fn own_keys(node: &OverlayNode) -> Result<Vec<String>> {
    assert_readable(&node.inner.context)?;
    if node_base(node).is_array() {
        let length = array_length(&array_overlay(node));
        let mut keys = Vec::with_capacity(length as usize + 1);
        for index in 0..length {
            keys.push(index.to_string());
        }
        keys.push("length".to_owned());
        return Ok(keys);
    }
    let base = node_base(node);
    let base_keys: Vec<String> = {
        let base_guard = lock(&base.inner.state);
        match &base_guard.value {
            Some(JsonValue::Object(map)) => map.keys().cloned().collect(),
            _ => Vec::new(),
        }
    };
    let mut keys = Vec::with_capacity(base_keys.len());
    let mut seen = IndexSet::with_capacity(base_keys.len());
    let guard = lock(&node.inner.state);
    for key in &base_keys {
        let deleted = guard.delete_key.as_deref() == Some(key.as_str())
            || guard.deletes.as_ref().is_some_and(|d| d.contains(key));
        let readded = guard.readded.as_ref().is_some_and(|r| r.contains(key));
        if !deleted && !readded && seen.insert(key.clone()) {
            keys.push(key.clone());
        }
    }
    if let Some(wk) = &guard.write_key {
        if seen.insert(wk.clone()) {
            keys.push(wk.clone());
        }
    }
    if let Some(writes) = &guard.writes {
        for key in writes.keys() {
            if seen.insert(key.clone()) {
                keys.push(key.clone());
            }
        }
    }
    drop(guard);

    let mut has_any_index = false;
    for key in &keys {
        if array_index(key).is_some() {
            has_any_index = true;
            break;
        }
    }
    if !has_any_index {
        return Ok(keys);
    }
    let mut indices: Vec<i64> = Vec::new();
    let mut strings: Vec<String> = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some(index) = array_index(&key) {
            indices.push(index);
        } else {
            strings.push(key);
        }
    }
    indices.sort_unstable();
    let mut ordered = Vec::with_capacity(indices.len() + strings.len());
    for idx in indices {
        ordered.push(idx.to_string());
    }
    ordered.extend(strings);
    Ok(ordered)
}

fn get_descriptor(node: &OverlayNode, property: PropertyKey) -> Result<Option<DraftPropertyDescriptor>> {
    assert_readable(&node.inner.context)?;
    let PropertyKey::String(key) = &property;
    if node_base(node).is_array() {
        if key == "length" {
            let len = array_length(&array_overlay(node));
            return Ok(Some(DraftPropertyDescriptor {
                configurable: false,
                enumerable: false,
                writable: true,
                value: DraftValue::Json(JsonValue::from(len)),
            }));
        }
        let Some(index) = array_index(key) else {
            return Ok(None);
        };
        if index >= array_length(&array_overlay(node)) {
            return Ok(None);
        }
    } else if !object_has(node, key) {
        return Ok(None);
    }
    let val = get_property(node, property)?;
    Ok(Some(DraftPropertyDescriptor {
        configurable: true,
        enumerable: true,
        writable: node.inner.context.inner.status.get() == Status::Open,
        value: val.into(),
    }))
}

fn base_object_has_own(node: &OverlayNode, key: &str) -> bool {
    let base = node_base(node);
    let base_guard = lock(&base.inner.state);
    match &base_guard.value {
        Some(JsonValue::Object(map)) => map.contains_key(key),
        _ => false,
    }
}

fn has_object_write(node: &OverlayNode, key: &str) -> bool {
    let guard = lock(&node.inner.state);
    guard.write_key.as_deref() == Some(key) || guard.writes.as_ref().is_some_and(|w| w.contains_key(key))
}

fn set_object_write(node: &OverlayNode, key: &str, value: Stored) {
    let context = &node.inner.context;
    let mut guard = lock(&node.inner.state);
    if let Some(writes) = guard.writes.take() {
        drop(guard);
        let reference = writes.get(key).cloned();
        let next_ref = match reference {
            None => store_value(context, value),
            Some(existing) => replace_stored_value(context, &existing, value),
        };
        let mut writes = writes;
        writes.insert(key.to_owned(), next_ref);
        lock(&node.inner.state).writes = Some(writes);
        return;
    }
    if guard.write_key.is_none() || guard.write_key.as_deref() == Some(key) {
        let existing_ref = guard.write_value.take();
        guard.write_key = Some(key.to_owned());
        drop(guard);
        let next_ref = match existing_ref {
            None => store_value(context, value),
            Some(existing) => replace_stored_value(context, &existing, value),
        };
        lock(&node.inner.state).write_value = Some(next_ref);
        return;
    }
    let prev_key = guard.write_key.take().unwrap_or_default();
    let prev_val = guard.write_value.take();
    drop(guard);
    let mut writes = IndexMap::new();
    if let Some(pv) = prev_val {
        writes.insert(prev_key, pv);
    }
    let next_ref = store_value(context, value);
    writes.insert(key.to_owned(), next_ref);
    lock(&node.inner.state).writes = Some(writes);
}

fn delete_object_write(node: &OverlayNode, key: &str) {
    let context = &node.inner.context;
    let mut guard = lock(&node.inner.state);
    if guard.write_key.as_deref() == Some(key) {
        guard.write_key = None;
        let prev = guard.write_value.take();
        drop(guard);
        if let Some(reference) = prev {
            release_stored_value(context, &reference);
        }
    } else if let Some(writes) = &mut guard.writes {
        let prev = writes.shift_remove(key);
        drop(guard);
        if let Some(reference) = prev {
            release_stored_value(context, &reference);
        }
    }
}

fn is_object_deleted(node: &OverlayNode, key: &str) -> bool {
    let guard = lock(&node.inner.state);
    guard.delete_key.as_deref() == Some(key) || guard.deletes.as_ref().is_some_and(|d| d.contains(key))
}

fn set_object_deletion(node: &OverlayNode, key: &str) {
    let mut guard = lock(&node.inner.state);
    if let Some(deletes) = &mut guard.deletes {
        deletes.insert(key.to_owned());
        return;
    }
    if guard.delete_key.is_none() || guard.delete_key.as_deref() == Some(key) {
        guard.delete_key = Some(key.to_owned());
        return;
    }
    let prev = guard.delete_key.take();
    let mut deletes = IndexSet::new();
    if let Some(p) = prev {
        deletes.insert(p);
    }
    deletes.insert(key.to_owned());
    guard.deletes = Some(deletes);
}

fn delete_object_deletion(node: &OverlayNode, key: &str) {
    let mut guard = lock(&node.inner.state);
    if guard.delete_key.as_deref() == Some(key) {
        guard.delete_key = None;
    } else if let Some(deletes) = &mut guard.deletes {
        deletes.shift_remove(key);
    }
}

fn object_has(node: &OverlayNode, key: &str) -> bool {
    if is_object_deleted(node, key) {
        return false;
    }
    has_object_write(node, key) || base_object_has_own(node, key)
}

fn object_value(node: &OverlayNode, key: &str) -> Option<Stored> {
    let context = &node.inner.context;
    let ref_opt = {
        let guard = lock(&node.inner.state);
        if guard.write_key.as_deref() == Some(key) {
            guard.write_value.clone()
        } else {
            guard.writes.as_ref().and_then(|w| w.get(key).cloned())
        }
    };
    if let Some(reference) = ref_opt {
        return Some(stored_value(context, &reference));
    }
    if let Some(c) = base_child_container(node, &ParentKey::String(key.to_owned())) {
        return Some(Stored::Container(c));
    }
    let base = node_base(node);
    let base_guard = lock(&base.inner.state);
    match &base_guard.value {
        Some(JsonValue::Object(map)) => map.get(key).cloned().map(Stored::Json),
        _ => None,
    }
}

fn mark_dirty(node: &OverlayNode) {
    {
        let mut guard = lock(&node.inner.state);
        if guard.dirty {
            return;
        }
        guard.dirty = true;
    }
    lock(&node.inner.context.inner.state).dirty.push(node.clone());
    let mut current = node_parent(node);
    while let Some(parent) = current {
        lock(&parent.inner.state).subtree_dirty = true;
        current = node_parent(&parent);
    }
}

fn array_overlay(node: &OverlayNode) -> ArrayOverlay {
    if let Some(existing) = lock(&node.inner.state).array.clone() {
        return existing;
    }
    let base_len = node_base(node).array_len() as i64;
    let overlay = ArrayOverlay {
        inner: Arc::new(Mutex::new(ArrayOverlayState {
            root: None,
            pieces: None,
            base_overrides: None,
            insert_overrides: None,
            structural: false,
            generation: 0,
            plan: None,
            seed: PIECE_PRIORITY_SEED,
            located_offset: 0,
            base_locations: None,
            insert_locations: None,
        })),
    };
    if base_len > 0 {
        let piece = Piece::base(0, base_len, 1);
        let root_node = create_piece_node(&overlay, piece);
        lock(&overlay.inner).root = Some(root_node);
    }
    let mut guard = lock(&node.inner.state);
    if let Some(existing) = &guard.array {
        return existing.clone();
    }
    guard.array = Some(overlay.clone());
    overlay
}

fn base_overrides_for_write(overlay: &ArrayOverlay) -> Arc<Mutex<IndexMap<i64, StoredRef>>> {
    let mut guard = lock(&overlay.inner);
    guard
        .base_overrides
        .get_or_insert_with(|| Arc::new(Mutex::new(IndexMap::new())))
        .clone()
}

fn insert_overrides_for_write(
    overlay: &ArrayOverlay,
) -> Arc<Mutex<IndexMap<InsertSource, Arc<Mutex<IndexMap<i64, StoredRef>>>>>> {
    let mut guard = lock(&overlay.inner);
    guard
        .insert_overrides
        .get_or_insert_with(|| Arc::new(Mutex::new(IndexMap::new())))
        .clone()
}

fn next_piece_priority(overlay: &ArrayOverlay) -> u32 {
    let mut guard = lock(&overlay.inner);
    let mut value = guard.seed;
    value ^= value << 13;
    value ^= value >> 17;
    value ^= value << 5;
    guard.seed = value;
    value
}

fn tree_elements(node: Option<&PieceNode>) -> i64 {
    node.map_or(0, |n| n.elements)
}

fn update_piece_node(node: &mut PieceNode) {
    node.elements = tree_elements(node.left.as_deref()) + node.piece.len() + tree_elements(node.right.as_deref());
}

fn create_piece_node(overlay: &ArrayOverlay, piece: Piece) -> Box<PieceNode> {
    let len = piece.len();
    Box::new(PieceNode {
        piece,
        left: None,
        right: None,
        priority: next_piece_priority(overlay),
        elements: len,
    })
}

fn merge_piece_trees(left: Option<Box<PieceNode>>, right: Option<Box<PieceNode>>) -> Option<Box<PieceNode>> {
    match (left, right) {
        (None, r) => r,
        (l, None) => l,
        (Some(mut l), Some(mut r)) => {
            if l.priority >= r.priority {
                l.right = merge_piece_trees(l.right.take(), Some(r));
                update_piece_node(&mut l);
                Some(l)
            } else {
                r.left = merge_piece_trees(Some(l), r.left.take());
                update_piece_node(&mut r);
                Some(r)
            }
        }
    }
}

fn split_piece_tree(
    overlay: &ArrayOverlay,
    root: Option<Box<PieceNode>>,
    index: i64,
) -> (Option<Box<PieceNode>>, Option<Box<PieceNode>>) {
    let Some(mut node) = root else {
        return (None, None);
    };
    let left_length = tree_elements(node.left.as_deref());
    if index < left_length {
        let (left, right) = split_piece_tree(overlay, node.left.take(), index);
        node.left = right;
        update_piece_node(&mut node);
        return (left, Some(node));
    }
    let piece_len = node.piece.len();
    let piece_end = left_length + piece_len;
    if index > piece_end {
        let (left, right) = split_piece_tree(overlay, node.right.take(), index - piece_end);
        node.right = left;
        update_piece_node(&mut node);
        return (Some(node), right);
    }
    if index == left_length {
        let left = node.left.take();
        update_piece_node(&mut node);
        return (left, Some(node));
    }
    if index == piece_end {
        let right = node.right.take();
        update_piece_node(&mut node);
        return (Some(node), right);
    }
    let offset = index - left_length;
    let data = node.piece.data();
    let (first, second) = match data {
        PieceData::Base { start, length, step } => (
            Piece::base(start, offset, step),
            Piece::base(start + step * offset, length - offset, step),
        ),
        PieceData::Insert {
            source,
            start,
            length,
            step,
        } => (
            Piece::insert(source.clone(), start, offset, step),
            Piece::insert(source, start + step * offset, length - offset, step),
        ),
    };
    (
        merge_piece_trees(node.left.take(), Some(create_piece_node(overlay, first))),
        merge_piece_trees(Some(create_piece_node(overlay, second)), node.right.take()),
    )
}

fn leftmost_piece_node(mut node: &PieceNode) -> &PieceNode {
    while let Some(left) = node.left.as_deref() {
        node = left;
    }
    node
}

fn rightmost_piece_node(mut node: &PieceNode) -> &PieceNode {
    while let Some(right) = node.right.as_deref() {
        node = right;
    }
    node
}

fn mergeable_pieces(left: &Piece, right: &Piece) -> bool {
    let l = left.data();
    let r = right.data();
    match (&l, &r) {
        (PieceData::Base { .. }, PieceData::Insert { .. }) | (PieceData::Insert { .. }, PieceData::Base { .. }) => {
            false
        }
        (PieceData::Insert { source: ls, .. }, PieceData::Insert { source: rs, .. }) if ls != rs => false,
        _ => {
            let l_len = l.length();
            let r_len = r.length();
            if l_len == 1 && r_len == 1 {
                return (r.start() - l.start()).abs() == 1;
            }
            l.step() == r.step() && l.start() + l.step() * l_len == r.start()
        }
    }
}

fn join_normalized(
    overlay: &ArrayOverlay,
    left: Option<Box<PieceNode>>,
    right: Option<Box<PieceNode>>,
) -> Option<Box<PieceNode>> {
    match (left, right) {
        (None, r) => r,
        (l, None) => l,
        (Some(l), Some(r)) => {
            let left_piece = rightmost_piece_node(&l).piece.clone();
            let right_piece = leftmost_piece_node(&r).piece.clone();
            if !mergeable_pieces(&left_piece, &right_piece) {
                return merge_piece_trees(Some(l), Some(r));
            }
            let ld = left_piece.data();
            let rd = right_piece.data();
            let l_total = l.elements;
            let (left_rest, _) = split_piece_tree(overlay, Some(l), l_total - ld.length());
            let (_, right_rest) = split_piece_tree(overlay, Some(r), rd.length());
            let step = if ld.length() == 1 {
                rd.start() - ld.start()
            } else {
                ld.step()
            };
            let combined = match ld {
                PieceData::Base { start, length, .. } => Piece::base(start, length + rd.length(), step),
                PieceData::Insert {
                    source, start, length, ..
                } => Piece::insert(source, start, length + rd.length(), step),
            };
            let combined_node = Some(create_piece_node(overlay, combined));
            let merged_left = join_normalized(overlay, left_rest, combined_node);
            join_normalized(overlay, merged_left, right_rest)
        }
    }
}

fn flatten_piece_tree(node: Option<&PieceNode>, output: &mut Vec<Piece>) {
    let Some(n) = node else {
        return;
    };
    flatten_piece_tree(n.left.as_deref(), output);
    output.push(n.piece.clone());
    flatten_piece_tree(n.right.as_deref(), output);
}

fn pieces_of(overlay: &ArrayOverlay) -> Vec<Piece> {
    let mut guard = lock(&overlay.inner);
    if let Some(pieces) = &guard.pieces {
        return pieces.clone();
    }
    let mut pieces = Vec::new();
    flatten_piece_tree(guard.root.as_deref(), &mut pieces);
    guard.pieces = Some(pieces.clone());
    pieces
}

fn tree_from_pieces(overlay: &ArrayOverlay, mut pieces: Vec<Piece>) -> Option<Box<PieceNode>> {
    merge_pieces(&mut pieces);
    let mut root = None;
    for piece in pieces {
        let node = create_piece_node(overlay, piece);
        root = merge_piece_trees(root, Some(node));
    }
    root
}

fn replace_all_pieces(overlay: &ArrayOverlay, pieces: Vec<Piece>) {
    let root = tree_from_pieces(overlay, pieces);
    let mut guard = lock(&overlay.inner);
    guard.root = root;
    guard.pieces = None;
    guard.base_locations = None;
    guard.insert_locations = None;
}

fn array_length(overlay: &ArrayOverlay) -> i64 {
    let guard = lock(&overlay.inner);
    tree_elements(guard.root.as_deref())
}

fn locate_piece(overlay: &ArrayOverlay, mut index: i64) -> Result<(Piece, i64)> {
    let mut guard = lock(&overlay.inner);
    let mut cursor = guard.root.as_deref();
    while let Some(node) = cursor {
        let left_length = tree_elements(node.left.as_deref());
        let piece_len = node.piece.len();
        if index < left_length {
            cursor = node.left.as_deref();
        } else if index >= left_length + piece_len {
            index -= left_length + piece_len;
            cursor = node.right.as_deref();
        } else {
            let located_offset = index - left_length;
            let piece = node.piece.clone();
            guard.located_offset = located_offset;
            return Ok((piece, located_offset));
        }
    }
    Err(pi_js::Error::js("RangeError", "Array overlay index is out of range"))
}

fn array_index(key: &str) -> Option<i64> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key == "0" {
        return Some(0);
    }
    let bytes = key.as_bytes();
    if bytes[0] < b'1' || bytes[0] > b'9' {
        return None;
    }
    let mut index = i64::from(bytes[0] - b'0');
    for &b in &bytes[1..] {
        if !b.is_ascii_digit() {
            return None;
        }
        index = index * 10 + i64::from(b - b'0');
        if index >= MAX_ARRAY_INDEX {
            return None;
        }
    }
    Some(index)
}

fn entry_value_at(node: &OverlayNode, piece: &Piece, source_index: i64) -> Stored {
    let overlay = array_overlay(node);
    let data = piece.data();
    match data {
        PieceData::Base { .. } => {
            let override_ref = lock(&overlay.inner)
                .base_overrides
                .as_ref()
                .and_then(|map| lock(map).get(&source_index).cloned());
            if let Some(reference) = override_ref {
                return stored_value(&node.inner.context, &reference);
            }
            if let Some(c) = base_child_container(node, &ParentKey::Index(source_index)) {
                return Stored::Container(c);
            }
            let base = node_base(node);
            let base_guard = lock(&base.inner.state);
            match &base_guard.value {
                Some(JsonValue::Array(arr)) => Stored::Json(arr[source_index as usize].clone()),
                _ => Stored::Json(JsonValue::Null),
            }
        }
        PieceData::Insert { source, .. } => {
            let override_ref = lock(&overlay.inner).insert_overrides.as_ref().and_then(|map| {
                let outer = lock(map);
                outer
                    .get(&source)
                    .and_then(|inner| lock(inner).get(&source_index).cloned())
            });
            let reference = override_ref.unwrap_or_else(|| source.get(source_index as usize));
            stored_value(&node.inner.context, &reference)
        }
    }
}

fn has_entry_override_at(overlay: &ArrayOverlay, piece: &Piece, source_index: i64) -> bool {
    let data = piece.data();
    let guard = lock(&overlay.inner);
    match data {
        PieceData::Base { .. } => guard
            .base_overrides
            .as_ref()
            .is_some_and(|map| lock(map).contains_key(&source_index)),
        PieceData::Insert { source, .. } => guard.insert_overrides.as_ref().is_some_and(|map| {
            lock(map)
                .get(&source)
                .is_some_and(|inner| lock(inner).contains_key(&source_index))
        }),
    }
}

fn extend_rightmost_piece(node: &mut PieceNode, amount: i64) {
    if let Some(right) = node.right.as_deref_mut() {
        extend_rightmost_piece(right, amount);
    } else {
        lock(&node.piece.inner).add_length(amount);
    }
    update_piece_node(node);
}

fn invalidate_piece_caches(guard: &mut ArrayOverlayState) {
    guard.pieces = None;
    guard.base_locations = None;
    guard.insert_locations = None;
    guard.plan = None;
}

fn replace_piece_range(node: &OverlayNode, index: i64, remove: i64, inserted: Vec<Piece>) {
    if remove == 0 && inserted.is_empty() {
        return;
    }
    let overlay = array_overlay(node);
    let len = array_length(&overlay);
    if remove == 0 && index == len && inserted.len() == 1 {
        let addition = &inserted[0];
        let add_data = addition.data();
        let mut fast_extended = false;
        if let PieceData::Insert {
            source: add_source,
            start: add_start,
            length: add_len,
            step: add_step,
        } = add_data
        {
            if add_step == 1 {
                let mut guard = lock(&overlay.inner);
                if let Some(root) = guard.root.as_deref_mut() {
                    let tail_data = rightmost_piece_node(root).piece.data();
                    if let PieceData::Insert {
                        source: tail_source,
                        start: tail_start,
                        length: tail_len,
                        step: tail_step,
                    } = tail_data
                    {
                        if tail_step == 1 && tail_start + tail_len == tail_source.len() {
                            tail_source.extend_from(&add_source, add_start, add_len);
                            extend_rightmost_piece(root, add_len);
                            invalidate_piece_caches(&mut guard);
                            guard.structural = true;
                            guard.generation += 1;
                            fast_extended = true;
                        }
                    }
                }
            }
        }
        if fast_extended {
            mark_dirty(node);
            return;
        }
    }

    let root = lock(&overlay.inner).root.take();
    let (left, rest) = split_piece_tree(&overlay, root, index);
    let (_, right) = split_piece_tree(&overlay, rest, remove);
    let middle = tree_from_pieces(&overlay, inserted);
    let joined_left = join_normalized(&overlay, left, middle);
    let next_root = join_normalized(&overlay, joined_left, right);
    {
        let mut guard = lock(&overlay.inner);
        guard.root = next_root;
        invalidate_piece_caches(&mut guard);
        guard.structural = true;
        guard.generation += 1;
    }
    mark_dirty(node);
}

fn merge_pieces(pieces: &mut Vec<Piece>) {
    let mut index = 1usize;
    while index < pieces.len() {
        let left_data = pieces[index - 1].data();
        let right_data = pieces[index].data();
        let same_source = match (&left_data, &right_data) {
            (PieceData::Base { .. }, PieceData::Base { .. }) => true,
            (PieceData::Insert { source: l, .. }, PieceData::Insert { source: r, .. }) => l == r,
            _ => false,
        };
        if same_source
            && left_data.length() == 1
            && right_data.length() == 1
            && (right_data.start() - left_data.start()).abs() == 1
        {
            let mut left_guard = lock(&pieces[index - 1].inner);
            left_guard.set_step(right_data.start() - left_data.start());
            left_guard.add_length(1);
            drop(left_guard);
            pieces.drain(index..=index);
        } else if same_source
            && left_data.step() == right_data.step()
            && left_data.start() + left_data.step() * left_data.length() == right_data.start()
        {
            lock(&pieces[index - 1].inner).add_length(right_data.length());
            pieces.drain(index..=index);
        } else {
            index += 1;
        }
    }
}

fn insert_piece(node: &OverlayNode, items: &[Stored]) -> Vec<Piece> {
    if items.is_empty() {
        return Vec::new();
    }
    let refs: Vec<StoredRef> = items
        .iter()
        .cloned()
        .map(|item| store_value(&node.inner.context, item))
        .collect();
    let len = refs.len() as i64;
    let source = InsertSource::new(refs);
    vec![Piece::insert(source, 0, len, 1)]
}

fn insert_placement_piece(node: &OverlayNode, values: &[Placement]) -> Result<Vec<Piece>> {
    if values.is_empty() {
        return Ok(Vec::new());
    }
    let stored_len = lock(&node.inner.context.inner.state).stored.len();
    let mut refs = Vec::with_capacity(values.len());
    for placement in values {
        match clone_placement(placement) {
            Ok(cloned) => refs.push(store_value(&node.inner.context, cloned)),
            Err(err) => {
                lock(&node.inner.context.inner.state).stored.truncate(stored_len);
                return Err(err);
            }
        }
    }
    let len = refs.len() as i64;
    let source = InsertSource::new(refs);
    Ok(vec![Piece::insert(source, 0, len, 1)])
}

fn equal_primitives(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Number(a), JsonValue::Number(b)) => equal_json_numbers(a, b),
        _ => false,
    }
}

fn equal_json_numbers(left: &serde_json::Number, right: &serde_json::Number) -> bool {
    if left == right {
        return true;
    }
    if let (Some(a), Some(b)) = (left.as_i64(), right.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (left.as_u64(), right.as_u64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (left.as_f64(), right.as_f64()) {
        return a == b;
    }
    false
}

fn set_array_index(node: &OverlayNode, index: i64, stored: Stored) -> Result<()> {
    let overlay = array_overlay(node);
    let len = array_length(&overlay);
    if index == len {
        let pieces = insert_piece(node, &[stored]);
        replace_piece_range(node, len, 0, pieces);
        return Ok(());
    }
    let (piece, located_offset) = locate_piece(&overlay, index)?;
    let data = piece.data();
    let source_index = data.start() + data.step() * located_offset;
    let current = entry_value_at(node, &piece, source_index);
    if !stored.is_container() {
        if let (Stored::Json(next_prim), Stored::Json(curr_prim)) = (&stored, &current) {
            if !is_container(curr_prim) && equal_primitives(curr_prim, next_prim) {
                return Ok(());
            }
        }
    }
    match data {
        PieceData::Base { .. } => {
            let existing_ref = lock(&overlay.inner)
                .base_overrides
                .as_ref()
                .and_then(|map| lock(map).get(&source_index).cloned());
            let equals_base = if !stored.is_container() {
                if let Stored::Json(next_prim) = &stored {
                    let base = node_base(node);
                    let base_guard = lock(&base.inner.state);
                    match &base_guard.value {
                        Some(JsonValue::Array(arr)) => {
                            let base_el = &arr[source_index as usize];
                            !is_container(base_el) && equal_primitives(base_el, next_prim)
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if equals_base {
                if let Some(existing) = &existing_ref {
                    release_stored_value(&node.inner.context, existing);
                }
                if let Some(map) = lock(&overlay.inner).base_overrides.clone() {
                    lock(&map).shift_remove(&source_index);
                }
            } else if let Some(existing) = existing_ref {
                let next_ref = replace_stored_value(&node.inner.context, &existing, stored);
                if let Some(map) = lock(&overlay.inner).base_overrides.clone() {
                    lock(&map).insert(source_index, next_ref);
                }
            } else {
                let next_ref = store_value(&node.inner.context, stored);
                let map = base_overrides_for_write(&overlay);
                lock(&map).insert(source_index, next_ref);
            }
        }
        PieceData::Insert { source, .. } => {
            let overrides_map_opt = lock(&overlay.inner)
                .insert_overrides
                .as_ref()
                .and_then(|outer| lock(outer).get(&source).cloned());
            let existing_ref = overrides_map_opt
                .as_ref()
                .and_then(|inner| lock(inner).get(&source_index).cloned());
            let equals_source = if !stored.is_container() {
                if let Stored::Json(next_prim) = &stored {
                    match stored_value(&node.inner.context, &source.get(source_index as usize)) {
                        Stored::Json(src_prim) => !is_container(&src_prim) && equal_primitives(&src_prim, next_prim),
                        Stored::Container(_) => false,
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if equals_source {
                if let Some(existing) = &existing_ref {
                    release_stored_value(&node.inner.context, existing);
                }
                if let Some(inner) = overrides_map_opt {
                    let mut inner_guard = lock(&inner);
                    inner_guard.shift_remove(&source_index);
                    let empty = inner_guard.is_empty();
                    drop(inner_guard);
                    if empty {
                        if let Some(outer) = lock(&overlay.inner).insert_overrides.clone() {
                            lock(&outer).shift_remove(&source);
                        }
                    }
                }
            } else {
                let inner = match overrides_map_opt {
                    Some(m) => m,
                    None => {
                        let m = Arc::new(Mutex::new(IndexMap::new()));
                        let outer = insert_overrides_for_write(&overlay);
                        lock(&outer).insert(source, m.clone());
                        m
                    }
                };
                let next_ref = match existing_ref {
                    Some(existing) => replace_stored_value(&node.inner.context, &existing, stored),
                    None => store_value(&node.inner.context, stored),
                };
                lock(&inner).insert(source_index, next_ref);
            }
        }
    }
    mark_dirty(node);
    Ok(())
}

fn set_array_length(node: &OverlayNode, next: i64) {
    let current = array_length(&array_overlay(node));
    if next == current {
        return;
    }
    if next < current {
        replace_piece_range(node, next, current - next, Vec::new());
    } else {
        let items = vec![Stored::Json(JsonValue::Null); (next - current) as usize];
        let pieces = insert_piece(node, &items);
        replace_piece_range(node, current, 0, pieces);
    }
}

fn to_array_length(value: &JsonValue) -> Result<i64> {
    if let Some(n) = value.as_i64() {
        if n >= 0 && n < MAX_ARRAY_LENGTH {
            return Ok(n);
        }
    } else if let Some(n) = value.as_f64() {
        if n.is_finite() && n.fract() == 0.0 && n >= 0.0 && n < MAX_ARRAY_LENGTH as f64 {
            return Ok(n as i64);
        }
    }
    Err(pi_js::Error::js("RangeError", "Invalid array length"))
}

fn clamp_index(value: i64, length: i64) -> i64 {
    if value < 0 {
        (length + value).max(0)
    } else {
        value.min(length)
    }
}

fn check_array_mutator_writable(node: &OverlayNode) -> Result<()> {
    if !node_base(node).is_array() {
        return Err(pi_js::Error::js(
            "TypeError",
            "Array mutator called on incompatible receiver",
        ));
    }
    assert_writable(&node.inner.context)
}

fn array_push(node: &OverlayNode, items: &[Placement]) -> Result<i64> {
    check_array_mutator_writable(node)?;
    let length = array_length(&array_overlay(node));
    let pieces = insert_placement_piece(node, items)?;
    replace_piece_range(node, length, 0, pieces);
    Ok(length + items.len() as i64)
}

fn array_pop(node: &OverlayNode) -> Result<GetValue> {
    check_array_mutator_writable(node)?;
    let length = array_length(&array_overlay(node));
    if length == 0 {
        return Ok(GetValue::Absent);
    }
    let value = get_array_index(node, length - 1)?;
    replace_piece_range(node, length - 1, 1, Vec::new());
    Ok(value)
}

fn array_shift(node: &OverlayNode) -> Result<GetValue> {
    check_array_mutator_writable(node)?;
    let length = array_length(&array_overlay(node));
    if length == 0 {
        return Ok(GetValue::Absent);
    }
    let value = get_array_index(node, 0)?;
    replace_piece_range(node, 0, 1, Vec::new());
    Ok(value)
}

fn array_unshift(node: &OverlayNode, items: &[Placement]) -> Result<i64> {
    check_array_mutator_writable(node)?;
    let pieces = insert_placement_piece(node, items)?;
    replace_piece_range(node, 0, 0, pieces);
    Ok(array_length(&array_overlay(node)))
}

fn array_splice(
    node: &OverlayNode,
    start_arg: Option<i64>,
    delete_count_arg: Option<i64>,
    items: &[Placement],
) -> Result<Vec<GetValue>> {
    check_array_mutator_writable(node)?;
    let length = array_length(&array_overlay(node));
    let start = match start_arg {
        None => 0,
        Some(s) => clamp_index(s, length),
    };
    let remove = match (start_arg, delete_count_arg) {
        (None, _) => 0,
        (Some(_), None) => length - start,
        (Some(_), Some(dc)) => dc.max(0).min(length - start),
    };
    let mut removed = Vec::with_capacity(remove as usize);
    for offset in 0..remove {
        removed.push(get_array_index(node, start + offset)?);
    }
    let pieces = insert_placement_piece(node, items)?;
    replace_piece_range(node, start, remove, pieces);
    Ok(removed)
}

fn array_reverse(node: &OverlayNode) -> Result<DraftOverlay> {
    check_array_mutator_writable(node)?;
    let overlay = array_overlay(node);
    if array_length(&overlay) < 2 {
        return Ok(DraftOverlay { node: node.clone() });
    }
    let mut pieces = pieces_of(&overlay);
    pieces.reverse();
    for piece in &pieces {
        lock(&piece.inner).reverse_in_place();
    }
    replace_all_pieces(&overlay, pieces);
    {
        let mut guard = lock(&overlay.inner);
        guard.structural = true;
        guard.generation += 1;
        guard.plan = None;
    }
    mark_dirty(node);
    Ok(DraftOverlay { node: node.clone() })
}

fn get_value_to_js_sort_string(value: &GetValue) -> String {
    match value {
        GetValue::Absent => String::new(),
        GetValue::Json(v) => json_to_js_sort_string(v),
        GetValue::Node(n) => {
            if let Ok(c) = clone_node(n) {
                if let Ok(v) = c.to_json() {
                    return json_to_js_sort_string(&v);
                }
            }
            "[object Object]".to_owned()
        }
    }
}

fn json_to_js_sort_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_owned(),
        JsonValue::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        JsonValue::Number(n) => n.as_f64().map_or_else(|| n.to_string(), pi_js::json::number_to_string),
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(arr) => {
            let parts: Vec<String> = arr
                .iter()
                .map(|item| {
                    if item.is_null() {
                        String::new()
                    } else {
                        json_to_js_sort_string(item)
                    }
                })
                .collect();
            parts.join(",")
        }
        JsonValue::Object(_) => "[object Object]".to_owned(),
    }
}

fn get_value_clone_for_sort(node: &OverlayNode, value: &GetValue) -> DraftValue {
    match value {
        GetValue::Absent => DraftValue::Undefined,
        GetValue::Json(v) => DraftValue::Json(v.clone()),
        GetValue::Node(n) => {
            let _ = node;
            DraftValue::Overlay(DraftOverlay { node: n.clone() })
        }
    }
}

fn array_sort<F>(node: &OverlayNode, mut comparator: Option<&mut F>) -> Result<DraftOverlay>
where
    F: FnMut(DraftValue, DraftValue) -> Result<Ordering>,
{
    check_array_mutator_writable(node)?;
    let overlay = array_overlay(node);
    let base_len = node_base(node).array_len();
    let mut inserted_sources: Vec<InsertSource> = Vec::new();
    let mut inserted_indices: Vec<i64> = Vec::new();
    let mut inserted_values: Vec<GetValue> = Vec::new();
    let mut base_values: Vec<Option<GetValue>> = (0..base_len).map(|_| None).collect();
    let mut order: Vec<i64> = Vec::new();

    for piece in pieces_of(&overlay) {
        let data = piece.data();
        for offset in 0..data.length() {
            let source_index = data.start() + data.step() * offset;
            match &data {
                PieceData::Base { .. } => {
                    order.push(source_index);
                    base_values[source_index as usize] = Some(public_sort_value(
                        node,
                        source_index,
                        &inserted_sources,
                        &inserted_indices,
                    ));
                }
                PieceData::Insert { source, .. } => {
                    inserted_sources.push(source.clone());
                    inserted_indices.push(source_index);
                    let token = -(inserted_sources.len() as i64);
                    order.push(token);
                    inserted_values.push(public_sort_value(node, token, &inserted_sources, &inserted_indices));
                }
            }
        }
    }

    let mut base_snapshot: IndexMap<i64, Stored> = IndexMap::new();
    let mut insert_snapshots: IndexMap<InsertSource, IndexMap<i64, Stored>> = IndexMap::new();
    {
        let ov_guard = lock(&overlay.inner);
        if let Some(base_ovr) = &ov_guard.base_overrides {
            for (&index, value_ref) in &*lock(base_ovr) {
                base_snapshot.insert(index, stored_value(&node.inner.context, value_ref));
            }
        }
        if let Some(ins_ovr) = &ov_guard.insert_overrides {
            for (source, overrides) in &*lock(ins_ovr) {
                let mut snapshot = IndexMap::new();
                for (&index, value_ref) in &*lock(overrides) {
                    snapshot.insert(index, stored_value(&node.inner.context, value_ref));
                }
                insert_snapshots.insert(source.clone(), snapshot);
            }
        }
    }

    let generation = lock(&overlay.inner).generation;
    let mut cmp_error: Option<pi_js::Error> = None;
    order.sort_by(|&left, &right| {
        if cmp_error.is_some() {
            return Ordering::Equal;
        }
        let left_val = if left < 0 {
            &inserted_values[(-left - 1) as usize]
        } else {
            base_values[left as usize].as_ref().unwrap_or(&GetValue::Absent)
        };
        let right_val = if right < 0 {
            &inserted_values[(-right - 1) as usize]
        } else {
            base_values[right as usize].as_ref().unwrap_or(&GetValue::Absent)
        };
        if let Some(cmp_fn) = comparator.as_deref_mut() {
            let lv = get_value_clone_for_sort(node, left_val);
            let rv = get_value_clone_for_sort(node, right_val);
            match cmp_fn(lv, rv) {
                Ok(ord) => ord,
                Err(err) => {
                    cmp_error = Some(err);
                    Ordering::Equal
                }
            }
        } else {
            let a = get_value_to_js_sort_string(left_val);
            let b = get_value_to_js_sort_string(right_val);
            a.cmp(&b)
        }
    });
    if let Some(err) = cmp_error {
        return Err(err);
    }

    let need_restore = {
        let ov_guard = lock(&overlay.inner);
        !base_snapshot.is_empty()
            || !insert_snapshots.is_empty()
            || ov_guard.base_overrides.as_ref().is_some_and(|m| !lock(m).is_empty())
            || ov_guard.insert_overrides.as_ref().is_some_and(|m| !lock(m).is_empty())
    };
    if need_restore {
        for &token in &order {
            restore_sort_override(
                node,
                token,
                &inserted_sources,
                &inserted_indices,
                &base_snapshot,
                &insert_snapshots,
            );
        }
    }

    let comparator_was_structural = lock(&overlay.inner).generation != generation;
    let current_length = array_length(&overlay);
    let same_prefix = current_length >= order.len() as i64
        && order
            .iter()
            .enumerate()
            .all(|(idx, &token)| same_sort_token_at(&overlay, idx as i64, token, &inserted_sources, &inserted_indices));
    if !same_prefix {
        let replacement = pieces_from_sort_order(&order, &inserted_sources, &inserted_indices);
        replace_piece_range(node, 0, (order.len() as i64).min(current_length), replacement);
    }
    if comparator_was_structural {
        deduplicate_array_entries(node)?;
    }
    Ok(DraftOverlay { node: node.clone() })
}

fn array_fill(
    node: &OverlayNode,
    supplied: &Placement,
    start_arg: Option<i64>,
    end_arg: Option<i64>,
) -> Result<DraftOverlay> {
    check_array_mutator_writable(node)?;
    let overlay = array_overlay(node);
    let length = array_length(&overlay);
    let start = start_arg.map_or(0, |s| clamp_index(s, length));
    let end = end_arg.map_or(length, |e| clamp_index(e, length));
    if end <= start {
        return Ok(DraftOverlay { node: node.clone() });
    }
    let count = (end - start) as usize;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(clone_placement(supplied)?);
    }
    let pieces = insert_piece(node, &items);
    replace_piece_range(node, start, end - start, pieces);
    Ok(DraftOverlay { node: node.clone() })
}

fn array_copy_within(
    node: &OverlayNode,
    target_arg: i64,
    start_arg: i64,
    end_arg: Option<i64>,
) -> Result<DraftOverlay> {
    check_array_mutator_writable(node)?;
    let overlay = array_overlay(node);
    let length = array_length(&overlay);
    let target = clamp_index(target_arg, length);
    let start = clamp_index(start_arg, length);
    let end = end_arg.map_or(length, |e| clamp_index(e, length));
    let count = (end - start).max(0).min(length - target);
    let mut values = Vec::with_capacity(count as usize);
    for offset in 0..count {
        let val = get_array_index(node, start + offset)?;
        let placement = match val {
            GetValue::Absent => Placement::Undefined,
            GetValue::Json(v) => Placement::Json(v),
            GetValue::Node(n) => Placement::Node(n),
        };
        values.push(clone_placement(&placement)?);
    }
    let pieces = insert_piece(node, &values);
    replace_piece_range(node, target, count, pieces);
    Ok(DraftOverlay { node: node.clone() })
}

fn sort_token_source<'a>(token: i64, sources: &'a [InsertSource]) -> Option<&'a InsertSource> {
    if token < 0 {
        Some(&sources[(-token - 1) as usize])
    } else {
        None
    }
}

fn sort_token_index(token: i64, indices: &[i64]) -> i64 {
    if token < 0 {
        indices[(-token - 1) as usize]
    } else {
        token
    }
}

fn public_sort_value(node: &OverlayNode, token: i64, sources: &[InsertSource], indices: &[i64]) -> GetValue {
    let overlay = array_overlay(node);
    let source = sort_token_source(token, sources);
    let source_index = sort_token_index(token, indices);
    let override_ref = {
        let guard = lock(&overlay.inner);
        match source {
            None => guard
                .base_overrides
                .as_ref()
                .and_then(|map| lock(map).get(&source_index).cloned()),
            Some(src) => guard.insert_overrides.as_ref().and_then(|outer| {
                lock(outer)
                    .get(src)
                    .and_then(|inner| lock(inner).get(&source_index).cloned())
            }),
        }
    };
    let stored = match override_ref {
        Some(r) => stored_value(&node.inner.context, &r),
        None => match source {
            None => {
                if let Some(c) = base_child_container(node, &ParentKey::Index(source_index)) {
                    Stored::Container(c)
                } else {
                    let base = node_base(node);
                    let base_guard = lock(&base.inner.state);
                    match &base_guard.value {
                        Some(JsonValue::Array(arr)) => Stored::Json(arr[source_index as usize].clone()),
                        _ => Stored::Json(JsonValue::Null),
                    }
                }
            }
            Some(src) => stored_value(&node.inner.context, &src.get(source_index as usize)),
        },
    };
    let has_ovr = match source {
        Some(_) => true,
        None => lock(&overlay.inner)
            .base_overrides
            .as_ref()
            .is_some_and(|map| lock(map).contains_key(&source_index)),
    };
    match stored {
        Stored::Json(v) if !is_container(&v) => GetValue::Json(v),
        Stored::Json(v) => GetValue::Node(create_node(
            &node.inner.context,
            Container::from_json(v),
            Some(node),
            if source.is_none() {
                ParentKind::BaseArrayEntry
            } else {
                ParentKind::InsertedArrayEntry
            },
            ParentKey::Index(source_index),
            source.cloned(),
            has_ovr,
        )),
        Stored::Container(c) => GetValue::Node(create_node(
            &node.inner.context,
            c,
            Some(node),
            if source.is_none() {
                ParentKind::BaseArrayEntry
            } else {
                ParentKind::InsertedArrayEntry
            },
            ParentKey::Index(source_index),
            source.cloned(),
            has_ovr,
        )),
    }
}

fn restore_sort_override(
    node: &OverlayNode,
    token: i64,
    sources: &[InsertSource],
    indices: &[i64],
    base_snapshot: &IndexMap<i64, Stored>,
    insert_snapshots: &IndexMap<InsertSource, IndexMap<i64, Stored>>,
) {
    let overlay = array_overlay(node);
    let source = sort_token_source(token, sources);
    let source_index = sort_token_index(token, indices);
    match source {
        None => {
            let existing_ref = lock(&overlay.inner)
                .base_overrides
                .as_ref()
                .and_then(|map| lock(map).get(&source_index).cloned());
            if let Some(snapshot_val) = base_snapshot.get(&source_index).cloned() {
                let next_ref = match existing_ref {
                    None => store_value(&node.inner.context, snapshot_val),
                    Some(existing) => replace_stored_value(&node.inner.context, &existing, snapshot_val),
                };
                let map = base_overrides_for_write(&overlay);
                lock(&map).insert(source_index, next_ref);
            } else {
                if let Some(existing) = existing_ref {
                    release_stored_value(&node.inner.context, &existing);
                }
                if let Some(map) = lock(&overlay.inner).base_overrides.clone() {
                    lock(&map).shift_remove(&source_index);
                }
            }
        }
        Some(src) => {
            let inner_opt = lock(&overlay.inner)
                .insert_overrides
                .as_ref()
                .and_then(|outer| lock(outer).get(src).cloned());
            let existing_ref = inner_opt
                .as_ref()
                .and_then(|inner| lock(inner).get(&source_index).cloned());
            let snapshot_val = insert_snapshots
                .get(src)
                .and_then(|snap| snap.get(&source_index))
                .cloned();
            if let Some(snap_val) = snapshot_val {
                let inner = match inner_opt {
                    Some(m) => m,
                    None => {
                        let m = Arc::new(Mutex::new(IndexMap::new()));
                        let outer = insert_overrides_for_write(&overlay);
                        lock(&outer).insert(src.clone(), m.clone());
                        m
                    }
                };
                let next_ref = match existing_ref {
                    None => store_value(&node.inner.context, snap_val),
                    Some(existing) => replace_stored_value(&node.inner.context, &existing, snap_val),
                };
                lock(&inner).insert(source_index, next_ref);
            } else {
                if let Some(existing) = existing_ref {
                    release_stored_value(&node.inner.context, &existing);
                }
                if let Some(inner) = inner_opt {
                    let mut inner_guard = lock(&inner);
                    inner_guard.shift_remove(&source_index);
                    let empty = inner_guard.is_empty();
                    drop(inner_guard);
                    if empty {
                        if let Some(outer) = lock(&overlay.inner).insert_overrides.clone() {
                            lock(&outer).shift_remove(src);
                        }
                    }
                }
            }
        }
    }
}

fn same_sort_token_at(
    overlay: &ArrayOverlay,
    logical_index: i64,
    token: i64,
    sources: &[InsertSource],
    indices: &[i64],
) -> bool {
    let Ok((piece, located_offset)) = locate_piece(overlay, logical_index) else {
        return false;
    };
    let data = piece.data();
    let source_index = data.start() + data.step() * located_offset;
    let token_source = sort_token_source(token, sources);
    if source_index != sort_token_index(token, indices) {
        return false;
    }
    match (&data, token_source) {
        (PieceData::Base { .. }, None) => true,
        (PieceData::Insert { source, .. }, Some(ts)) => source == ts,
        _ => false,
    }
}

fn deduplicate_array_entries(node: &OverlayNode) -> Result<()> {
    let overlay = array_overlay(node);
    let mut seen_base = IndexSet::new();
    let mut seen_insert: IndexMap<InsertSource, IndexSet<i64>> = IndexMap::new();
    let mut next: Vec<Piece> = Vec::new();
    let mut duplicated = false;
    for piece in pieces_of(&overlay) {
        let data = piece.data();
        for offset in 0..data.length() {
            let source_index = data.start() + data.step() * offset;
            let seen = match &data {
                PieceData::Base { .. } => !seen_base.insert(source_index),
                PieceData::Insert { source, .. } => {
                    let indices = seen_insert.entry(source.clone()).or_default();
                    !indices.insert(source_index)
                }
            };
            if seen {
                duplicated = true;
                let val = entry_value_at(node, &piece, source_index);
                let cloned = clone_placement_stored(&val, &node.inner.context)?;
                let inserted = insert_piece(node, &[cloned]);
                if let Some(first) = inserted.into_iter().next() {
                    append_merged_piece(&mut next, first);
                }
            } else {
                append_merged_piece(&mut next, singleton_piece(&piece, source_index));
            }
        }
    }
    if !duplicated {
        return Ok(());
    }
    replace_all_pieces(&overlay, next);
    {
        let mut guard = lock(&overlay.inner);
        guard.structural = true;
        guard.generation += 1;
        guard.plan = None;
    }
    mark_dirty(node);
    Ok(())
}

fn pieces_from_sort_order(order: &[i64], sources: &[InsertSource], indices: &[i64]) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    for &token in order {
        let source = sort_token_source(token, sources);
        let source_index = sort_token_index(token, indices);
        if let Some(previous) = pieces.last() {
            let mut prev_guard = lock(&previous.inner);
            let same_source = match (&*prev_guard, source) {
                (PieceData::Base { .. }, None) => true,
                (PieceData::Insert { source: ps, .. }, Some(s)) => ps == s,
                _ => false,
            };
            if same_source {
                if prev_guard.length() == 1 {
                    let step = source_index - prev_guard.start();
                    if step == 1 || step == -1 {
                        prev_guard.set_step(step);
                        prev_guard.add_length(1);
                        continue;
                    }
                } else if prev_guard.start() + prev_guard.step() * prev_guard.length() == source_index {
                    prev_guard.add_length(1);
                    continue;
                }
            }
        }
        pieces.push(match source {
            None => Piece::base(source_index, 1, 1),
            Some(src) => Piece::insert(src.clone(), source_index, 1, 1),
        });
    }
    pieces
}

fn append_merged_piece(pieces: &mut Vec<Piece>, piece: Piece) {
    let pd = piece.data();
    if let Some(previous) = pieces.last() {
        let mut prev_guard = lock(&previous.inner);
        let same_source = match (&*prev_guard, &pd) {
            (PieceData::Base { .. }, PieceData::Base { .. }) => true,
            (PieceData::Insert { source: ls, .. }, PieceData::Insert { source: rs, .. }) => ls == rs,
            _ => false,
        };
        if same_source && prev_guard.length() == 1 && pd.length() == 1 {
            let step = pd.start() - prev_guard.start();
            if step == 1 || step == -1 {
                prev_guard.set_step(step);
                prev_guard.add_length(1);
                return;
            }
        }
        if same_source
            && prev_guard.step() == pd.step()
            && prev_guard.start() + prev_guard.step() * prev_guard.length() == pd.start()
        {
            prev_guard.add_length(pd.length());
            return;
        }
    }
    pieces.push(piece);
}

fn singleton_piece(piece: &Piece, source_index: i64) -> Piece {
    match piece.data() {
        PieceData::Base { .. } => Piece::base(source_index, 1, 1),
        PieceData::Insert { source, .. } => Piece::insert(source, source_index, 1, 1),
    }
}

fn clone_placement(value: &Placement) -> Result<Stored> {
    match value {
        Placement::Undefined => Err(pi_js::Error::js(
            "TypeError",
            "Value cannot contain undefined; values must be strict JSON",
        )),
        Placement::NonFiniteFloat => Err(pi_js::Error::js(
            "TypeError",
            "Value contains a non-finite number and is not strict JSON",
        )),
        Placement::Json(v) => {
            let copied = crate::json::copy_json(v, None)?;
            if is_container(&copied) {
                Ok(Stored::Container(Container::from_json(copied)))
            } else {
                Ok(Stored::Json(copied))
            }
        }
        Placement::Node(node) => clone_placement_node(node).map(Stored::Container),
    }
}

fn clone_node(node: &OverlayNode) -> Result<Container> {
    assert_readable(&node.inner.context)?;
    if node_base(node).is_array() {
        let overlay = array_overlay(node);
        let mut result = Vec::with_capacity(array_length(&overlay) as usize);
        for piece in pieces_of(&overlay) {
            let data = piece.data();
            for offset in 0..data.length() {
                let source_index = data.start() + data.step() * offset;
                let val = entry_value_at(node, &piece, source_index);
                let cloned = clone_stored(&val, &node.inner.context)?;
                result.push(cloned.into_json()?);
            }
        }
        return Ok(Container::from_json(JsonValue::Array(result)));
    }
    let keys = own_keys(node)?;
    let mut map = serde_json::Map::with_capacity(keys.len());
    for key in keys {
        let Some(val) = object_value(node, &key) else {
            continue;
        };
        let cloned = clone_stored(&val, &node.inner.context)?;
        map.insert(key, cloned.into_json()?);
    }
    Ok(Container::from_json(JsonValue::Object(map)))
}

fn clone_stored(value: &Stored, context: &OverlayContext) -> Result<Stored> {
    match value {
        Stored::Json(v) => Ok(Stored::Json(v.clone())),
        Stored::Container(c) => {
            let node_opt = lock(&context.inner.state).raw_nodes.get(c).cloned();
            let Some(node) = node_opt else {
                return Ok(Stored::Container(c.clone()));
            };
            let is_modified = {
                let guard = lock(&node.inner.state);
                guard.dirty || guard.subtree_dirty
            };
            if !is_modified {
                Ok(Stored::Container(c.clone()))
            } else {
                Ok(Stored::Container(clone_node(&node)?))
            }
        }
    }
}

fn clone_placement_node(node: &OverlayNode) -> Result<Container> {
    assert_readable(&node.inner.context)?;
    if node_base(node).is_array() {
        let overlay = array_overlay(node);
        let mut result = Vec::with_capacity(array_length(&overlay) as usize);
        for piece in pieces_of(&overlay) {
            let data = piece.data();
            for offset in 0..data.length() {
                let source_index = data.start() + data.step() * offset;
                let val = entry_value_at(node, &piece, source_index);
                let cloned = clone_placement_stored(&val, &node.inner.context)?;
                result.push(cloned.into_json()?);
            }
        }
        return Ok(Container::from_json(JsonValue::Array(result)));
    }
    let keys = own_keys(node)?;
    let mut map = serde_json::Map::with_capacity(keys.len());
    for key in keys {
        let Some(val) = object_value(node, &key) else {
            continue;
        };
        let cloned = clone_placement_stored(&val, &node.inner.context)?;
        map.insert(key, cloned.into_json()?);
    }
    Ok(Container::from_json(JsonValue::Object(map)))
}

fn clone_placement_stored(value: &Stored, context: &OverlayContext) -> Result<Stored> {
    match value {
        Stored::Json(v) => Ok(Stored::Json(crate::json::copy_json(v, None)?)),
        Stored::Container(c) => {
            let node_opt = lock(&context.inner.state).raw_nodes.get(c).cloned();
            match node_opt {
                None => {
                    let json = c.to_json()?;
                    let copied = crate::json::copy_json(&json, None)?;
                    Ok(Stored::Container(Container::from_json(copied)))
                }
                Some(node) => Ok(Stored::Container(clone_placement_node(&node)?)),
            }
        }
    }
}

fn emit_operations(context: &OverlayContext) -> Result<Vec<Op>> {
    if let Some(simple) = emit_simple_object_operations(context)? {
        return Ok(simple);
    }
    let mut operations: Vec<Op> = Vec::new();
    let mut forced_folds: IndexMap<OverlayNode, Path> = IndexMap::new();
    let mut emission_paths: IndexMap<OverlayNode, Path> = IndexMap::new();
    let mut dense_indices: IndexMap<OverlayNode, DenseCandidates> = IndexMap::new();

    let dirty_nodes = lock(&context.inner.state).dirty.clone();
    for node in &dirty_nodes {
        record_dense_array_position(node, &mut dense_indices);
        if node_base(node).is_array() {
            let overlay = array_overlay(node);
            let (structural, base_overrides) = {
                let guard = lock(&overlay.inner);
                (guard.structural, guard.base_overrides.clone())
            };
            if !structural {
                if let Some(map) = base_overrides {
                    let map_guard = lock(&map);
                    if !map_guard.is_empty() {
                        let base_len = node_base(node).array_len();
                        let candidates = dense_indices.entry(node.clone()).or_insert_with(|| DenseCandidates {
                            indices: Vec::new(),
                            bits: None,
                            length: base_len,
                        });
                        for &index in map_guard.keys() {
                            add_dense_candidate(candidates, index);
                        }
                    }
                }
            }
        }
    }

    let mut dense_regions: IndexMap<OverlayNode, Vec<DenseRegion>> = IndexMap::new();
    for (array, candidates) in &dense_indices {
        let Some(path) = resolve_path(array) else {
            continue;
        };
        if path_has_reserved_segment(&path) {
            continue;
        }
        let regions = build_dense_regions(candidates);
        if !regions.is_empty() {
            dense_regions.insert(array.clone(), regions);
        }
    }

    for node in &dirty_nodes {
        let Some(path) = resolve_path(node) else {
            continue;
        };
        if has_covering_dense_region(node, &path, &dense_regions) {
            continue;
        }
        emission_paths.insert(node.clone(), path.clone());
        if !node_base(node).is_array() && has_reserved_mutation(node) {
            forced_folds.insert(node.clone(), path.clone());
        }
        if let Some(reserved_at) = path
            .iter()
            .position(|seg| matches!(seg, Seg::String(s) if RESERVED_SEGMENTS.contains(s.as_str())))
        {
            let mut ancestor = node.clone();
            for _ in (reserved_at + 1..=path.len()).rev() {
                if let Some(p) = node_parent(&ancestor) {
                    ancestor = p;
                }
            }
            forced_folds.insert(ancestor, path[..reserved_at].to_vec());
        }
    }

    for (node, path) in &forced_folds {
        emission_paths.insert(node.clone(), path.clone());
    }
    for node in dense_regions.keys() {
        if let Some(path) = resolve_path(node) {
            if !has_covering_dense_region(node, &path, &dense_regions) {
                emission_paths.insert(node.clone(), path);
            }
        }
    }

    let max_depth = emission_paths.values().map(Vec::len).max().unwrap_or(0);
    let mut buckets: Vec<Vec<OverlayNode>> = vec![Vec::new(); max_depth + 1];
    for (node, path) in &emission_paths {
        buckets[path.len()].push(node.clone());
    }

    let mut folded: IndexSet<OverlayNode> = IndexSet::new();
    for bucket in buckets {
        for node in bucket {
            let path = emission_paths[&node].clone();
            if has_placement_ancestor(&node) || has_folded_ancestor(&node, &folded) {
                continue;
            }
            if forced_folds.contains_key(&node) {
                let cloned = clone_node(&node)?.to_json()?;
                emit_set(&mut operations, &path, cloned);
                folded.insert(node);
                continue;
            }
            if node_base(&node).is_array() {
                emit_array_operations(
                    &node,
                    &path,
                    &mut operations,
                    dense_regions.get(&node).map(Vec::as_slice),
                )?;
            } else {
                emit_object_operations(&node, &path, &mut operations)?;
            }
            if operations.len() > MAX_DELTA_OPERATIONS {
                let root = lock(&context.inner.state)
                    .root
                    .clone()
                    .ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))?;
                let full = clone_node(&root)?.to_json()?;
                return Ok(vec![Op::Replace(OpReplace(ReplaceVerb::R, full))]);
            }
        }
    }
    Ok(operations)
}

fn path_has_reserved_segment(path: &Path) -> bool {
    path.iter()
        .any(|seg| matches!(seg, Seg::String(s) if RESERVED_SEGMENTS.contains(s.as_str())))
}

fn emit_simple_object_operations(context: &OverlayContext) -> Result<Option<Vec<Op>>> {
    let dirty_nodes = lock(&context.inner.state).dirty.clone();
    let mut nodes: Vec<(OverlayNode, Path)> = Vec::new();
    for node in dirty_nodes {
        if node_base(&node).is_array() || has_reserved_mutation(&node) || has_placement_ancestor(&node) {
            return Ok(None);
        }
        let mut parent = node_parent(&node);
        while let Some(p) = parent {
            if node_base(&p).is_array() {
                return Ok(None);
            }
            parent = node_parent(&p);
        }
        let Some(path) = resolve_path(&node) else {
            continue;
        };
        if path_has_reserved_segment(&path) {
            return Ok(None);
        }
        nodes.push((node, path));
        if nodes.len() > MAX_SIMPLE_OBJECT_NODES {
            return Ok(None);
        }
    }
    // Stable insertion sort avoids comparator/bucket allocations for the usual
    // handful of dirty object nodes.
    for index in 1..nodes.len() {
        let node = nodes[index].clone();
        let depth = node.1.len();
        let mut at = index;
        while at > 0 && nodes[at - 1].1.len() > depth {
            nodes[at] = nodes[at - 1].clone();
            at -= 1;
        }
        nodes[at] = node;
    }

    let mut operations = Vec::new();
    let mut can_materialize_directly = true;
    for (node, path) in &nodes {
        if emit_object_operations(node, path, &mut operations)? {
            can_materialize_directly = false;
        }
        if operations.len() > MAX_DELTA_OPERATIONS {
            let root = lock(&context.inner.state)
                .root
                .clone()
                .ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))?;
            let full = clone_node(&root)?.to_json()?;
            return Ok(Some(vec![Op::Replace(OpReplace(ReplaceVerb::R, full))]));
        }
    }
    lock(&context.inner.state).simple_object_materialization = can_materialize_directly;
    Ok(Some(operations))
}

fn locate_dense_array_position(node: &OverlayNode) -> Option<(OverlayNode, i64)> {
    let mut child = node.clone();
    while let Some(parent) = node_parent(&child) {
        if node_base(&parent).is_array() {
            let overlay = array_overlay(&parent);
            if child.inner.parent_kind != ParentKind::BaseArrayEntry || lock(&overlay.inner).structural {
                return None;
            }
            let ParentKey::Index(source_index) = child.inner.parent_key else {
                return None;
            };
            let override_ref = lock(&overlay.inner)
                .base_overrides
                .as_ref()
                .and_then(|map| lock(map).get(&source_index).cloned());
            let current = match override_ref {
                Some(r) => match stored_value(&parent.inner.context, &r) {
                    Stored::Container(c) => Some(c),
                    Stored::Json(_) => None,
                },
                None => base_child_container(&parent, &ParentKey::Index(source_index)),
            };
            if current.as_ref() != Some(&node_base(&child)) {
                return None;
            }
            return Some((parent, source_index));
        }
        child = parent;
    }
    None
}

fn has_covering_dense_region(
    node: &OverlayNode,
    path: &Path,
    dense_regions: &IndexMap<OverlayNode, Vec<DenseRegion>>,
) -> bool {
    let mut parent = node_parent(node);
    while let Some(p) = parent {
        if let Some(regions) = dense_regions.get(&p) {
            if let Some(parent_path) = resolve_path(&p) {
                if path.len() > parent_path.len() && path[..parent_path.len()] == parent_path[..] {
                    if let Seg::Index(idx) = path[parent_path.len()] {
                        if region_containing(regions, idx) {
                            return true;
                        }
                    }
                }
            }
        }
        parent = node_parent(&p);
    }
    false
}

fn add_dense_candidate(candidates: &mut DenseCandidates, index: i64) {
    if let Some(bits) = &mut candidates.bits {
        bits[index as usize] = 1;
        return;
    }
    candidates.indices.push(index);
    if candidates.indices.len() < DENSE_THRESHOLD {
        return;
    }
    let mut bits = vec![0u8; candidates.length];
    for &existing in &candidates.indices {
        bits[existing as usize] = 1;
    }
    candidates.bits = Some(bits);
    candidates.indices.clear();
}

fn record_dense_array_position(node: &OverlayNode, groups: &mut IndexMap<OverlayNode, DenseCandidates>) {
    let Some((array, index)) = locate_dense_array_position(node) else {
        return;
    };
    let array_len = node_base(&array).array_len();
    let candidates = groups.entry(array).or_insert_with(|| DenseCandidates {
        indices: Vec::new(),
        bits: None,
        length: array_len,
    });
    add_dense_candidate(candidates, index);
}

fn build_dense_regions(candidates: &DenseCandidates) -> Vec<DenseRegion> {
    let Some(bits) = &candidates.bits else {
        return Vec::new();
    };
    let mut regions = Vec::new();
    let mut at = 0usize;
    while at < bits.len() {
        while at < bits.len() && bits[at] == 0 {
            at += 1;
        }
        if at == bits.len() {
            break;
        }
        let start = at;
        let mut end = at;
        let mut count = 0usize;
        let mut gap = 0usize;
        while at < bits.len() {
            if bits[at] != 0 {
                count += 1;
                end = at;
                gap = 0;
            } else {
                gap += 1;
                if gap > 1 {
                    break;
                }
            }
            at += 1;
        }
        let length = end - start + 1;
        if count >= DENSE_THRESHOLD && count * 2 >= length {
            regions.push(DenseRegion {
                start: start as i64,
                length: length as i64,
            });
        }
    }
    regions
}

fn region_containing(regions: &[DenseRegion], index: i64) -> bool {
    regions.iter().any(|r| index >= r.start && index < r.start + r.length)
}

fn has_reserved_mutation(node: &OverlayNode) -> bool {
    let guard = lock(&node.inner.state);
    if let Some(wk) = &guard.write_key {
        if RESERVED_SEGMENTS.contains(wk.as_str()) {
            return true;
        }
    }
    if let Some(dk) = &guard.delete_key {
        if RESERVED_SEGMENTS.contains(dk.as_str()) {
            return true;
        }
    }
    if let Some(writes) = &guard.writes {
        if writes.keys().any(|k| RESERVED_SEGMENTS.contains(k.as_str())) {
            return true;
        }
    }
    if let Some(deletes) = &guard.deletes {
        if deletes.iter().any(|k| RESERVED_SEGMENTS.contains(k.as_str())) {
            return true;
        }
    }
    false
}

fn has_folded_ancestor(node: &OverlayNode, folded: &IndexSet<OverlayNode>) -> bool {
    let mut parent = node_parent(node);
    while let Some(p) = parent {
        if folded.contains(&p) {
            return true;
        }
        parent = node_parent(&p);
    }
    false
}

fn has_placement_ancestor(node: &OverlayNode) -> bool {
    let mut current = Some(node.clone());
    while let Some(c) = current {
        let parent = node_parent(&c);
        if parent.is_some() && c.inner.parent_placement {
            return true;
        }
        current = parent;
    }
    false
}

fn emit_object_write(
    node: &OverlayNode,
    path: &Path,
    operations: &mut Vec<Op>,
    key: &str,
    value: &Stored,
) -> Result<bool> {
    let mut next_path = path.clone();
    next_path.push(Seg::String(key.to_owned()));
    let is_readded = lock(&node.inner.state)
        .readded
        .as_ref()
        .is_some_and(|r| r.contains(key));
    let before = if is_readded {
        None
    } else {
        let base_val = node_base_json(node);
        match base_val {
            JsonValue::Object(mut map) => map.shift_remove(key),
            _ => None,
        }
    };
    let after = clone_stored(value, &node.inner.context)?.into_json()?;
    let emitted = emit_changed_value(operations, &next_path, before.as_ref(), &after)?;
    Ok(before.as_ref().is_some_and(is_container) && is_container(&after) && !emitted)
}

fn emit_object_operations(node: &OverlayNode, path: &Path, operations: &mut Vec<Op>) -> Result<bool> {
    // Immutable replay must encode delete-and-readd explicitly so string-key
    // insertion order matches the draft.
    let (readded, write_key, writes, delete_key, deletes) = {
        let guard = lock(&node.inner.state);
        (
            guard.readded.clone(),
            guard.write_key.clone(),
            guard.writes.clone(),
            guard.delete_key.clone(),
            guard.deletes.clone(),
        )
    };
    if let Some(readded_set) = readded {
        for key in readded_set {
            if base_object_has_own(node, &key) {
                let mut del_path = path.clone();
                del_path.push(Seg::String(key));
                operations.push(Op::Delete(OpDelete(DeleteVerb::D, del_path)));
            }
        }
    }
    let mut normalized_container_write = false;
    if let Some(wk) = write_key {
        if let Some(val) = object_value(node, &wk) {
            if emit_object_write(node, path, operations, &wk, &val)? {
                normalized_container_write = true;
            }
        }
    }
    if let Some(writes_map) = writes {
        for (key, value_ref) in writes_map {
            if operations.len() > MAX_DELTA_OPERATIONS {
                return Ok(normalized_container_write);
            }
            let val = stored_value(&node.inner.context, &value_ref);
            if emit_object_write(node, path, operations, &key, &val)? {
                normalized_container_write = true;
            }
        }
    }
    if let Some(dk) = delete_key {
        if base_object_has_own(node, &dk) {
            let mut del_path = path.clone();
            del_path.push(Seg::String(dk));
            operations.push(Op::Delete(OpDelete(DeleteVerb::D, del_path)));
        }
    }
    if let Some(deletes_set) = deletes {
        for key in deletes_set {
            if operations.len() > MAX_DELTA_OPERATIONS {
                return Ok(normalized_container_write);
            }
            if base_object_has_own(node, &key) {
                let mut del_path = path.clone();
                del_path.push(Seg::String(key));
                operations.push(Op::Delete(OpDelete(DeleteVerb::D, del_path)));
            }
        }
    }
    Ok(normalized_container_write)
}

fn build_array_plan(node: &OverlayNode) -> ArrayPlan {
    let overlay = array_overlay(node);
    if let Some(plan) = lock(&overlay.inner).plan.clone() {
        return plan;
    }
    let base_len = node_base(node).array_len();
    let pieces = pieces_of(&overlay);
    let mut retained = vec![0u8; base_len];
    let mut target_base = Vec::new();
    for piece in &pieces {
        let data = piece.data();
        if let PieceData::Base { start, length, step } = data {
            for offset in 0..length {
                let index = start + step * offset;
                retained[index as usize] = 1;
                target_base.push(index);
            }
        }
    }

    let mut remove_runs = Vec::new();
    let mut end = base_len;
    while end > 0 {
        if retained[end - 1] != 0 {
            end -= 1;
            continue;
        }
        let mut start = end - 1;
        while start > 0 && retained[start - 1] == 0 {
            start -= 1;
        }
        remove_runs.push(start as i64);
        remove_runs.push((end - start) as i64);
        end = start;
    }

    let mut retained_base = Vec::new();
    for (index, &flag) in retained.iter().enumerate() {
        if flag != 0 {
            retained_base.push(index as i64);
        }
    }

    let permutation = if target_base.iter().zip(retained_base.iter()).any(|(a, b)| a != b) {
        let mut positions = IndexMap::with_capacity(retained_base.len());
        for (pos, &val) in retained_base.iter().enumerate() {
            positions.insert(val, pos as i64);
        }
        Some(target_base.iter().map(|val| positions[val]).collect())
    } else {
        None
    };

    let mut insert_runs = Vec::new();
    let mut logical_index = 0i64;
    let mut piece_index = 0usize;
    while piece_index < pieces.len() {
        let len = pieces[piece_index].len();
        if pieces[piece_index].data().is_base() {
            logical_index += len;
            piece_index += 1;
            continue;
        }
        let start_piece = piece_index;
        while piece_index < pieces.len() && !pieces[piece_index].data().is_base() {
            logical_index += pieces[piece_index].len();
            piece_index += 1;
        }
        let run_len = range_length(&pieces, start_piece, piece_index);
        insert_runs.push(logical_index - run_len);
        insert_runs.push(start_piece as i64);
        insert_runs.push(piece_index as i64);
    }

    let plan = ArrayPlan {
        remove_runs,
        permutation,
        insert_runs,
    };
    lock(&overlay.inner).plan = Some(plan.clone());
    plan
}

fn range_length(pieces: &[Piece], start: usize, end: usize) -> i64 {
    let mut length = 0i64;
    for piece in &pieces[start..end] {
        length += piece.len();
    }
    length
}

fn emit_array_operations(
    node: &OverlayNode,
    path: &Path,
    operations: &mut Vec<Op>,
    dense_regions: Option<&[DenseRegion]>,
) -> Result<()> {
    let overlay = array_overlay(node);
    if let Some(regions) = dense_regions {
        for region in regions {
            if operations.len() > MAX_DELTA_OPERATIONS {
                return Ok(());
            }
            let cloned = clone_array_region(node, region)?;
            operations.push(Op::Splice(OpSplice(
                SpliceVerb::P,
                path.clone(),
                region.start,
                region.length,
                cloned,
            )));
        }
    }
    if lock(&overlay.inner).structural {
        let plan = build_array_plan(node);
        let mut idx = 0usize;
        while idx < plan.remove_runs.len() {
            if operations.len() > MAX_DELTA_OPERATIONS {
                return Ok(());
            }
            operations.push(Op::Splice(OpSplice(
                SpliceVerb::P,
                path.clone(),
                plan.remove_runs[idx],
                plan.remove_runs[idx + 1],
                Vec::new(),
            )));
            idx += 2;
        }
        if let Some(perm) = &plan.permutation {
            operations.push(Op::Move(OpMove(MoveVerb::M, path.clone(), perm.clone())));
        }
        let pieces = pieces_of(&overlay);
        let mut run = 0usize;
        while run < plan.insert_runs.len() {
            if operations.len() > MAX_DELTA_OPERATIONS {
                return Ok(());
            }
            let logical_index = plan.insert_runs[run];
            let start_piece = plan.insert_runs[run + 1] as usize;
            let end_piece = plan.insert_runs[run + 2] as usize;
            let mut items = Vec::new();
            for piece in &pieces[start_piece..end_piece] {
                let data = piece.data();
                for offset in 0..data.length() {
                    let source_index = data.start() + data.step() * offset;
                    let val = entry_value_at(node, piece, source_index);
                    items.push(clone_stored(&val, &node.inner.context)?.into_json()?);
                }
            }
            operations.push(Op::Splice(OpSplice(
                SpliceVerb::P,
                path.clone(),
                logical_index,
                0,
                items,
            )));
            run += 3;
        }
    }

    let base_overrides_snapshot: Vec<(i64, StoredRef)> = {
        let ov_guard = lock(&overlay.inner);
        match &ov_guard.base_overrides {
            Some(map) => lock(map).iter().map(|(&k, v)| (k, v.clone())).collect(),
            None => Vec::new(),
        }
    };
    if !base_overrides_snapshot.is_empty() {
        let base_val = node_base_json(node);
        let JsonValue::Array(base_arr) = base_val else {
            return Ok(());
        };
        for (base_index, value_ref) in base_overrides_snapshot {
            if operations.len() > MAX_DELTA_OPERATIONS {
                return Ok(());
            }
            let Some(index) = find_entry_index(&overlay, ParentKind::BaseArrayEntry, base_index, None) else {
                continue;
            };
            if dense_regions.is_some_and(|regions| region_containing(regions, index)) {
                continue;
            }
            let mut item_path = path.clone();
            item_path.push(Seg::Index(index));
            let val = stored_value(&node.inner.context, &value_ref);
            let after = clone_stored(&val, &node.inner.context)?.into_json()?;
            emit_changed_value(operations, &item_path, base_arr.get(base_index as usize), &after)?;
        }
    }
    Ok(())
}

fn clone_array_region(node: &OverlayNode, region: &DenseRegion) -> Result<Vec<JsonValue>> {
    let overlay = array_overlay(node);
    let mut result = Vec::with_capacity(region.length as usize);
    for index in region.start..region.start + region.length {
        let (piece, located_offset) = locate_piece(&overlay, index)?;
        let data = piece.data();
        let source_index = data.start() + data.step() * located_offset;
        let val = entry_value_at(node, &piece, source_index);
        result.push(clone_stored(&val, &node.inner.context)?.into_json()?);
    }
    Ok(result)
}

fn emit_changed_value(
    operations: &mut Vec<Op>,
    path: &NonEmptyPath,
    before: Option<&JsonValue>,
    after: &JsonValue,
) -> Result<bool> {
    if let Some(b) = before {
        if !is_container(after) && !is_container(b) && equal_primitives(b, after) {
            return Ok(false);
        }
        if is_container(b) && is_container(after) && equal_trusted_json(b, after) {
            return Ok(false);
        }
        if let (JsonValue::String(b_str), JsonValue::String(a_str)) = (b, after) {
            // Slice/equality stays on V8's rope fast path; startsWith scans the entire
            // accumulated prefix on every append.
            if a_str.len() > b_str.len() && a_str.starts_with(b_str.as_str()) {
                operations.push(Op::Append(OpAppend(
                    AppendVerb::A,
                    path.clone(),
                    a_str[b_str.len()..].to_owned(),
                )));
                return Ok(true);
            }
            let shared = overlap(b_str, a_str, STRING_OVERLAP_SCAN, None, None);
            if shared > 0 {
                let b_len16 = pi_js::str16::len(b_str) as i64;
                let a_len16 = pi_js::str16::len(a_str) as i64;
                operations.push(Op::Truncate(OpTruncate(
                    TruncateVerb::T,
                    path.clone(),
                    b_len16 - shared,
                )));
                if a_len16 > shared {
                    operations.push(Op::Append(OpAppend(
                        AppendVerb::A,
                        path.clone(),
                        pi_js::str16::slice(a_str, shared, None),
                    )));
                }
                return Ok(true);
            }
        }
    }
    operations.push(Op::Set(OpSet(SetVerb::S, path.clone(), after.clone())));
    Ok(true)
}

fn emit_set(operations: &mut Vec<Op>, path: &Path, value: JsonValue) {
    if path.is_empty() {
        operations.push(Op::Replace(OpReplace(ReplaceVerb::R, value)));
    } else {
        operations.push(Op::Set(OpSet(SetVerb::S, path.clone(), value)));
    }
}

fn resolve_path(node: &OverlayNode) -> Option<Path> {
    if let Some(cached) = lock(&node.inner.state).prepared_path.clone() {
        return Some(cached);
    }
    let Some(parent) = node_parent(node) else {
        let empty = Vec::new();
        lock(&node.inner.state).prepared_path = Some(empty.clone());
        return Some(empty);
    };
    let mut parent_path = resolve_path(&parent)?;
    if node.inner.parent_kind == ParentKind::Object {
        let ParentKey::String(key) = &node.inner.parent_key else {
            return None;
        };
        if !object_has(&parent, key) {
            return None;
        }
        let val = object_value(&parent, key);
        let matches = matches!(val, Some(Stored::Container(c)) if c == node_base(node));
        if !matches {
            return None;
        }
        parent_path.push(Seg::String(key.clone()));
        lock(&node.inner.state).prepared_path = Some(parent_path.clone());
        return Some(parent_path);
    }
    let overlay = array_overlay(&parent);
    let ParentKey::Index(source_index) = node.inner.parent_key else {
        return None;
    };
    let index = find_entry_index(
        &overlay,
        node.inner.parent_kind,
        source_index,
        node.inner.parent_source.as_ref(),
    )?;
    let (piece, _) = locate_piece(&overlay, index).ok()?;
    let val = entry_value_at(&parent, &piece, source_index);
    let matches = matches!(val, Stored::Container(c) if c == node_base(node));
    if !matches {
        return None;
    }
    parent_path.push(Seg::Index(index));
    lock(&node.inner.state).prepared_path = Some(parent_path.clone());
    Some(parent_path)
}

fn ensure_piece_locations(overlay: &ArrayOverlay) {
    if lock(&overlay.inner).base_locations.is_some() {
        return;
    }
    let pieces = pieces_of(overlay);
    let mut base_locations = Vec::new();
    let mut insert_locations: IndexMap<InsertSource, Vec<PieceLocation>> = IndexMap::new();
    let mut logical_start = 0i64;

    for piece in pieces {
        let data = piece.data();
        let last = data.start() + data.step() * (data.length() - 1);
        let len = data.length();
        let location = PieceLocation {
            piece: piece.clone(),
            logical_start,
            minimum: data.start().min(last),
            maximum: data.start().max(last),
        };
        match data {
            PieceData::Base { .. } => base_locations.push(location),
            PieceData::Insert { source, .. } => {
                insert_locations.entry(source).or_default().push(location);
            }
        }
        logical_start += len;
    }
    base_locations.sort_by_key(|loc| loc.minimum);
    for locs in insert_locations.values_mut() {
        locs.sort_by_key(|loc| loc.minimum);
    }
    let mut guard = lock(&overlay.inner);
    if guard.base_locations.is_none() {
        guard.base_locations = Some(base_locations);
        guard.insert_locations = Some(insert_locations);
    }
}

fn find_entry_index(
    overlay: &ArrayOverlay,
    kind: ParentKind,
    source_index: i64,
    source: Option<&InsertSource>,
) -> Option<i64> {
    if kind == ParentKind::BaseArrayEntry && !lock(&overlay.inner).structural {
        return Some(source_index);
    }
    ensure_piece_locations(overlay);
    let guard = lock(&overlay.inner);
    let locations: &[PieceLocation] = if kind == ParentKind::BaseArrayEntry {
        guard.base_locations.as_deref()?
    } else {
        let src = source?;
        guard.insert_locations.as_ref()?.get(src)?.as_slice()
    };
    let mut low = 0usize;
    let mut high = locations.len();
    while low < high {
        let middle = (low + high) / 2;
        if locations[middle].minimum <= source_index {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    if low == 0 {
        return None;
    }
    let location = &locations[low - 1];
    if source_index > location.maximum {
        return None;
    }
    let data = location.piece.data();
    let delta = source_index - data.start();
    let step = data.step();
    if delta % step != 0 {
        return None;
    }
    let offset = delta / step;
    if offset >= 0 && offset < data.length() {
        Some(location.logical_start + offset)
    } else {
        None
    }
}

fn ensure_operations(context: &OverlayContext) -> Result<Vec<Op>> {
    if let Some(ops) = lock(&context.inner.state).ops.clone() {
        return Ok(ops);
    }
    assert_readable(context)?;
    let ops = if context.inner.replacement {
        let tracker_base = context
            .inner
            .tracker
            .upgrade()
            .and_then(|t| t.replacement_base_json(context));
        let root_node = lock(&context.inner.state)
            .root
            .clone()
            .ok_or_else(|| pi_js::Error::js("TypeError", "Cannot use a settled overlay"))?;
        let root_json = node_base(&root_node).to_json()?;
        let noop = tracker_base
            .as_ref()
            .is_some_and(|base| equal_trusted_json(base, &root_json));
        lock(&context.inner.state).replacement_noop = noop;
        if noop {
            Vec::new()
        } else {
            vec![Op::Replace(OpReplace(ReplaceVerb::R, root_json))]
        }
    } else {
        emit_operations(context)?
    };
    lock(&context.inner.state).ops = Some(ops.clone());
    Ok(ops)
}

fn equal_trusted_json(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::Number(a), JsonValue::Number(b)) => equal_json_numbers(a, b),
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| equal_trusted_json(x, y))
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|other| equal_trusted_json(v, other)))
        }
        _ => false,
    }
}

fn abort_context(context: &OverlayContext) {
    let status = context.inner.status.get();
    if matches!(status, Status::Aborted | Status::Consumed | Status::Stale) {
        return;
    }
    context.inner.status.set(Status::Aborted);
    clear_context(context);
}

fn release_overlay_references(context: &OverlayContext) {
    if let Some(tracker) = context.inner.tracker.upgrade() {
        tracker.release_context(context);
    }
    release_overlay_references_only(context);
}

fn release_overlay_references_only(context: &OverlayContext) {
    let mut guard = lock(&context.inner.state);
    guard.registry_ref = None;
    guard.overlay_released = true;
    guard.dirty.clear();
    guard.nodes.clear();
    guard.raw_nodes.clear();
    guard.ops = None;
    guard.root = None;
    guard.bases.clear();
    guard.stored.clear();
    guard.base_value = None;
}

fn clear_context(context: &OverlayContext) {
    if let Some(tracker) = context.inner.tracker.upgrade() {
        tracker.release_context(context);
    }
    let nodes = {
        let mut guard = lock(&context.inner.state);
        guard.registry_ref = None;
        guard.dirty.clear();
        guard.raw_nodes.clear();
        guard.ops = None;
        guard.root = None;
        guard.bases.clear();
        guard.stored.clear();
        guard.base_value = None;
        guard.overlay_released = true;
        std::mem::take(&mut guard.nodes)
    };
    for node in nodes {
        let mut ng = lock(&node.inner.state);
        if let Some(array) = ng.array.take() {
            let mut ag = lock(&array.inner);
            ag.root = None;
            ag.pieces = None;
            ag.base_locations = None;
            ag.insert_locations = None;
            if let Some(bo) = &ag.base_overrides {
                lock(bo).clear();
            }
            if let Some(io) = &ag.insert_overrides {
                lock(io).clear();
            }
            ag.base_overrides = None;
            ag.insert_overrides = None;
        }
        ng.target = RELEASED.clone();
        ng.write_key = None;
        ng.write_value = None;
        ng.writes = None;
        ng.delete_key = None;
        ng.deletes = None;
        ng.readded = None;
        ng.base_object_children.clear();
        ng.base_array_children.clear();
        ng.subtree_dirty = false;
        ng.prepared_path = None;
    }
}

// PORT: TS `unknown`. JSON uses `JsonValue`; an overlay proxy is `Placement::Node` / `GetValue::Node`.
fn is_container(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}
