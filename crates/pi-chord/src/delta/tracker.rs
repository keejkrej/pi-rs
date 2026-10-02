//! Port of packages/chord/src/delta/tracker.ts

pub use crate::delta::Op;
pub use crate::delta::draft::Draft;
pub use crate::types::JsonValue;

use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use indexmap::{IndexMap, IndexSet};

use crate::delta::{NonEmptyPath, Path};

use pi_js::Result;

// PORT: JS object identity is `Arc` pointer equality (`IndexMap` / `IndexSet`, never `HashMap` /
// `HashSet`). `Symbol("chord.delta.overlay.node")` is [`ContainerState::node`]. The prepared-object
// `WeakMap` is [`Prepared::context`]. `WeakRef<TrackerImpl<object>>` is `Weak<dyn TrackerControl>`
// because the public `T` is erased on the overlay. Module `let` slots are process-wide mutexes.
// Proxy traps are the `get_property` / `set_property` / … functions; array methods are not returned
// as JS function values (`GetValue::Mutator`). `DATA_DESCRIPTOR` field order is the constant literal
// (`value`, `writable`, `enumerable`, `configurable`); `getDescriptor` builds
// `configurable`, `enumerable`, `writable`, `value`.

const MAX_DELTA_OPERATIONS: i64 = 4_096;
const MAX_SIMPLE_OBJECT_NODES: i64 = 128;
const MAX_RETAINED_NODES: i64 = 4_096;
const INITIAL_PRUNE_BUDGET: i64 = 256;
const PIECE_PRIORITY_SEED: u32 = 0x9e3779b9;
const STRING_OVERLAP_SCAN: i64 = 65_536;
const DENSE_THRESHOLD: i64 = 256;
const MAX_ARRAY_INDEX: i64 = 4_294_967_295;
const MAX_ARRAY_LENGTH: i64 = 4_294_967_296;

static ARRAY_MUTATORS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "push",
        "pop",
        "shift",
        "unshift",
        "splice",
        "reverse",
        "sort",
        "fill",
        "copyWithin",
    ])
});

static DATA_DESCRIPTOR: Mutex<DataDescriptor> = Mutex::new(DataDescriptor {
    value: None,
    writable: true,
    enumerable: true,
    configurable: true,
});

static RELEASED: LazyLock<Container> = LazyLock::new(|| Container {
    inner: Arc::new(ContainerInner {
        state: Mutex::new(ContainerState {
            value: None,
            node: None,
        }),
    }),
});

static LOCATED_DENSE_ARRAY: LazyLock<Mutex<Option<OverlayNode>>> = LazyLock::new(|| Mutex::new(None));
static LOCATED_DENSE_INDEX: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(0));

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
enum Status {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "prepared")]
    Prepared,
    #[serde(rename = "consumed")]
    Consumed,
    #[serde(rename = "aborted")]
    Aborted,
    #[serde(rename = "stale")]
    Stale,
}

// PORT: `{ value }` is shared with `Change` after the context is cleared, so the cell is its own handle.
#[derive(Clone)]
struct StatusCell {
    inner: Arc<Mutex<Status>>,
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

#[derive(Clone, Debug)]
enum ParentKey {
    String(String),
    Index(i64),
}

enum PropertyKey {
    String(String),
    Symbol,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ArrayMutatorKind {
    Push,
    Pop,
    Shift,
    Unshift,
    Splice,
    Reverse,
    Sort,
    Fill,
    CopyWithin,
}

struct DataDescriptor {
    value: Option<JsonValue>,
    writable: bool,
    enumerable: bool,
    configurable: bool,
}

struct PropertyDescriptor {
    configurable: bool,
    enumerable: bool,
    writable: bool,
    value: Option<GetValue>,
}

#[derive(Clone)]
enum Placement {
    Json(serde_json::Value),
    Node(OverlayNode),
}

enum GetValue {
    Absent,
    Json(JsonValue),
    Node(OverlayNode),
    Mutator(ArrayMutatorKind),
}

type Comparator = Arc<dyn Fn(&Placement, &Placement) -> f64 + Send + Sync>;

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
    /// PORT: `NODE` symbol slot.
    node: Option<Weak<OverlayNodeInner>>,
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

enum Stored {
    Json(JsonValue),
    Container(Container),
}

enum StoredRef {
    Json(JsonValue),
    Index { index: i64 },
}

#[derive(Clone)]
struct InsertSource {
    inner: Arc<InsertSourceInner>,
}

struct InsertSourceInner {
    refs: Mutex<Vec<StoredRef>>,
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

struct PieceNode {
    piece: Piece,
    left: Option<Box<PieceNode>>,
    right: Option<Box<PieceNode>>,
    priority: u32,
    elements: i64,
}

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
    length: i64,
}

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
    base_index: i64,
    parent_index: i64,
    parent_kind: ParentKind,
    parent_key: ParentKey,
    parent_placement: bool,
    state: Mutex<OverlayNodeState>,
}

struct OverlayNodeState {
    parent_source: Option<InsertSource>,
    target: Container,
    proxy: Container,
    write_key: Option<String>,
    write_value: Option<StoredRef>,
    writes: Option<IndexMap<String, StoredRef>>,
    delete_key: Option<String>,
    deletes: Option<IndexSet<String>>,
    readded: Option<IndexSet<String>>,
    array: Option<ArrayOverlay>,
    dirty: Option<bool>,
    subtree_dirty: Option<bool>,
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
    bases: Option<Vec<Container>>,
    stored: Option<Vec<Container>>,
    dirty: Vec<OverlayNode>,
    nodes: Vec<OverlayNode>,
    raw_nodes: Option<IndexMap<Container, OverlayNode>>,
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
    fn release_context(&self, _context: &OverlayContext) {
        todo!("port: Tracker::release_context");
    }

    fn replacement_base_json(&self, _context: &OverlayContext) -> Option<JsonValue> {
        todo!("port: Tracker::replacement_base");
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
        todo!("port: Prepared::abort");
    }
}

#[derive(Clone)]
pub struct Change<T> {
    inner: Arc<ChangeInner<T>>,
}

impl<T> Change<T> {
    pub fn state(&self) -> Result<Draft<T>> {
        todo!("port: Change::state");
    }

    pub fn prepare(&self) -> Result<Prepared<T>> {
        todo!("port: Change::prepare");
    }

    pub fn abort(&self) {
        todo!("port: Change::abort");
    }
}

#[derive(Clone)]
pub struct Tracker<T> {
    inner: Arc<TrackerInner<T>>,
}

impl<T> Tracker<T> {
    // PORT: the root is swapped under a mutex, so this clones instead of returning a JS reference.
    pub fn value(&self) -> T
    where
        T: Clone,
    {
        todo!("port: Tracker::value");
    }

    pub fn revision(&self) -> i64 {
        todo!("port: Tracker::revision");
    }

    pub fn begin_change(&self) -> Change<T> {
        todo!("port: Tracker::begin_change");
    }

    pub fn prepare_replace(&self, _value: T) -> Result<Prepared<T>> {
        todo!("port: Tracker::prepare_replace");
    }

    pub fn adopt(&self, _prepared: &Prepared<T>) -> Result<()> {
        todo!("port: Tracker::adopt");
    }

    fn release_context(&self, _context: &OverlayContext) {
        todo!("port: Tracker::release_context");
    }

    fn register(&self, _context: &OverlayContext) {
        todo!("port: Tracker::register");
    }

    fn prune(&self) {
        todo!("port: Tracker::prune");
    }

    // Adoption must not walk every node touched by a competing draft. Drop the
    // context's strong overlay references in O(1); externally retained proxies
    // still see the stale status and are reclaimed with their holders.
    fn invalidate(&self, _winner: &OverlayContext) {
        todo!("port: Tracker::invalidate");
    }

    fn replacement_base(&self, _context: &OverlayContext) -> Option<T>
    where
        T: Clone,
    {
        todo!("port: Tracker::replacement_base");
    }
}

/// Take immutable ownership of an alias-free strict-JSON root in O(1).
pub fn track<T>(_initial: T) -> Tracker<T> {
    todo!("port: track");
}

fn materialize_prepared<T>(_context: &OverlayContext) -> Result<Prepared<T>> {
    // Operation payloads are already detached from the mutable overlay. The
    // resulting revision and public batch intentionally share those payloads under
    // the trusted immutable-transfer contract.
    todo!("port: materialize_prepared");
}

fn materialize_operations<T>(_base: T, _operations: &[Op]) -> Result<T> {
    // Native splice/permutation remains faster for large structural batches.
    todo!("port: materialize_operations");
}

fn create_context(
    _tracker: Weak<dyn TrackerControl>,
    _owner: Arc<()>,
    _base_revision: i64,
    _root: Container,
    _replacement: bool,
    _base: JsonValue,
) -> OverlayContext {
    todo!("port: create_context");
}

fn create_node(
    _context: &OverlayContext,
    _base: Container,
    _parent: Option<&OverlayNode>,
    _parent_kind: Option<ParentKind>,
    _parent_key: Option<ParentKey>,
    _parent_source: Option<InsertSource>,
    _parent_placement: Option<bool>,
) -> OverlayNode {
    todo!("port: create_node");
}

fn node_base(_node: &OverlayNode) -> Container {
    todo!("port: node_base");
}

fn node_parent(_node: &OverlayNode) -> Option<OverlayNode> {
    todo!("port: node_parent");
}

fn store_value(_context: &OverlayContext, _value: Stored) -> StoredRef {
    todo!("port: store_value");
}

fn stored_value(_context: &OverlayContext, _reference: StoredRef) -> Stored {
    todo!("port: stored_value");
}

fn replace_stored_value(_context: &OverlayContext, _reference: StoredRef, _value: Stored) -> StoredRef {
    todo!("port: replace_stored_value");
}

fn release_stored_value(_context: &OverlayContext, _reference: StoredRef) {
    todo!("port: release_stored_value");
}

fn node_for_target(_target: &Container) -> Result<OverlayNode> {
    todo!("port: node_for_target");
}

fn is_settled_context(_context: &OverlayContext) -> bool {
    todo!("port: is_settled_context");
}

fn assert_readable(_context: &OverlayContext) -> Result<()> {
    todo!("port: assert_readable");
}

fn assert_writable(_context: &OverlayContext) -> Result<()> {
    todo!("port: assert_writable");
}

fn get_property(_node: &OverlayNode, _property: PropertyKey) -> Result<GetValue> {
    todo!("port: get_property");
}

fn get_array_index(_node: &OverlayNode, _index: i64) -> Result<GetValue> {
    todo!("port: get_array_index");
}

fn set_property(_node: &OverlayNode, _property: PropertyKey, _supplied: Placement) -> Result<bool> {
    todo!("port: set_property");
}

fn delete_property(_node: &OverlayNode, _property: PropertyKey) -> Result<bool> {
    todo!("port: delete_property");
}

fn has_property(_node: &OverlayNode, _property: PropertyKey) -> Result<bool> {
    todo!("port: has_property");
}

fn own_keys(_node: &OverlayNode) -> Result<Vec<String>> {
    todo!("port: own_keys");
}

fn get_descriptor(_node: &OverlayNode, _property: PropertyKey) -> Result<Option<PropertyDescriptor>> {
    todo!("port: get_descriptor");
}

fn has_object_write(_node: &OverlayNode, _key: &str) -> bool {
    todo!("port: has_object_write");
}

fn set_object_write(_node: &OverlayNode, _key: &str, _value: Stored) {
    todo!("port: set_object_write");
}

fn delete_object_write(_node: &OverlayNode, _key: &str) {
    todo!("port: delete_object_write");
}

fn is_object_deleted(_node: &OverlayNode, _key: &str) -> bool {
    todo!("port: is_object_deleted");
}

fn set_object_deletion(_node: &OverlayNode, _key: &str) {
    todo!("port: set_object_deletion");
}

fn delete_object_deletion(_node: &OverlayNode, _key: &str) {
    todo!("port: delete_object_deletion");
}

fn object_has(_node: &OverlayNode, _key: &str) -> bool {
    todo!("port: object_has");
}

fn object_value(_node: &OverlayNode, _key: &str) -> Stored {
    todo!("port: object_value");
}

fn mark_dirty(_node: &OverlayNode) {
    todo!("port: mark_dirty");
}

fn array_overlay(_node: &OverlayNode) -> ArrayOverlay {
    todo!("port: array_overlay");
}

fn base_overrides_for_write(_overlay: &ArrayOverlay) -> Arc<Mutex<IndexMap<i64, StoredRef>>> {
    todo!("port: base_overrides_for_write");
}

fn insert_overrides_for_write(
    _overlay: &ArrayOverlay,
) -> Arc<Mutex<IndexMap<InsertSource, Arc<Mutex<IndexMap<i64, StoredRef>>>>>> {
    todo!("port: insert_overrides_for_write");
}

fn next_piece_priority(_overlay: &ArrayOverlay) -> u32 {
    todo!("port: next_piece_priority");
}

fn tree_elements(_node: Option<&PieceNode>) -> i64 {
    todo!("port: tree_elements");
}

fn update_piece_node(_node: &mut PieceNode) {
    todo!("port: update_piece_node");
}

fn create_piece_node(_overlay: &ArrayOverlay, _piece: Piece) -> Box<PieceNode> {
    todo!("port: create_piece_node");
}

fn merge_piece_trees(_left: Option<Box<PieceNode>>, _right: Option<Box<PieceNode>>) -> Option<Box<PieceNode>> {
    todo!("port: merge_piece_trees");
}

fn split_piece_tree(
    _overlay: &ArrayOverlay,
    _root: Option<Box<PieceNode>>,
    _index: i64,
) -> (Option<Box<PieceNode>>, Option<Box<PieceNode>>) {
    todo!("port: split_piece_tree");
}

fn leftmost_piece_node(_node: &PieceNode) -> &PieceNode {
    todo!("port: leftmost_piece_node");
}

fn rightmost_piece_node(_node: &PieceNode) -> &PieceNode {
    todo!("port: rightmost_piece_node");
}

fn mergeable_pieces(_left: &Piece, _right: &Piece) -> bool {
    todo!("port: mergeable_pieces");
}

fn join_normalized(
    _overlay: &ArrayOverlay,
    _left: Option<Box<PieceNode>>,
    _right: Option<Box<PieceNode>>,
) -> Option<Box<PieceNode>> {
    todo!("port: join_normalized");
}

fn flatten_piece_tree(_node: Option<&PieceNode>, _output: &mut Vec<Piece>) {
    todo!("port: flatten_piece_tree");
}

fn pieces_of(_overlay: &ArrayOverlay) -> Vec<Piece> {
    todo!("port: pieces_of");
}

fn tree_from_pieces(_overlay: &ArrayOverlay, _pieces: Vec<Piece>) -> Option<Box<PieceNode>> {
    todo!("port: tree_from_pieces");
}

fn replace_all_pieces(_overlay: &ArrayOverlay, _pieces: Vec<Piece>) {
    todo!("port: replace_all_pieces");
}

fn array_length(_overlay: &ArrayOverlay) -> i64 {
    todo!("port: array_length");
}

fn locate_piece(_overlay: &ArrayOverlay, _index: i64) -> Result<Piece> {
    todo!("port: locate_piece");
}

fn array_index(_property: &PropertyKey) -> Option<i64> {
    todo!("port: array_index");
}

fn entry_value_at(_node: &OverlayNode, _piece: &Piece, _source_index: i64) -> Stored {
    todo!("port: entry_value_at");
}

fn has_entry_override_at(_overlay: &ArrayOverlay, _piece: &Piece, _source_index: i64) -> bool {
    todo!("port: has_entry_override_at");
}

fn extend_rightmost_piece(_node: &mut PieceNode, _amount: i64) {
    todo!("port: extend_rightmost_piece");
}

fn invalidate_piece_caches(_overlay: &ArrayOverlay) {
    todo!("port: invalidate_piece_caches");
}

fn replace_piece_range(_node: &OverlayNode, _index: i64, _remove: i64, _inserted: Vec<Piece>) {
    todo!("port: replace_piece_range");
}

fn merge_pieces(_pieces: &mut Vec<Piece>) {
    todo!("port: merge_pieces");
}

fn insert_piece(_node: &OverlayNode, _items: &[Stored], _start: Option<i64>) -> Vec<Piece> {
    todo!("port: insert_piece");
}

fn insert_placement_piece(_node: &OverlayNode, _values: &mut [Placement], _start: Option<i64>) -> Result<Vec<Piece>> {
    todo!("port: insert_placement_piece");
}

fn set_array_index(_node: &OverlayNode, _index: i64, _stored: Stored) -> Result<()> {
    todo!("port: set_array_index");
}

fn set_array_length(_node: &OverlayNode, _next: i64) -> Result<()> {
    todo!("port: set_array_length");
}

fn to_array_length(_value: &serde_json::Value) -> Result<i64> {
    todo!("port: to_array_length");
}

fn to_integer_or_infinity(_value: &serde_json::Value) -> f64 {
    todo!("port: to_integer_or_infinity");
}

fn clamp_index(_value: f64, _length: i64) -> i64 {
    todo!("port: clamp_index");
}

fn mutator_node(_receiver: &Placement) -> Result<Option<OverlayNode>> {
    todo!("port: mutator_node");
}

fn array_push(_receiver: &Placement, _items: &[Placement]) -> Result<i64> {
    todo!("port: array_push");
}

fn array_pop(_receiver: &Placement) -> Result<GetValue> {
    todo!("port: array_pop");
}

fn array_shift(_receiver: &Placement) -> Result<GetValue> {
    todo!("port: array_shift");
}

fn array_unshift(_receiver: &Placement, _items: &[Placement]) -> Result<i64> {
    todo!("port: array_unshift");
}

fn array_splice(_receiver: &Placement, _args: &[Placement]) -> Result<Vec<GetValue>> {
    todo!("port: array_splice");
}

fn array_reverse(_receiver: &Placement) -> Result<GetValue> {
    todo!("port: array_reverse");
}

fn array_sort(_receiver: &Placement, _comparator: Option<&Comparator>) -> Result<GetValue> {
    todo!("port: array_sort");
}

fn array_fill(
    _receiver: &Placement,
    _supplied: &Placement,
    _start: Option<&serde_json::Value>,
    _end: Option<&serde_json::Value>,
) -> Result<GetValue> {
    todo!("port: array_fill");
}

fn array_copy_within(
    _receiver: &Placement,
    _target: &serde_json::Value,
    _start: &serde_json::Value,
    _end: Option<&serde_json::Value>,
) -> Result<GetValue> {
    todo!("port: array_copy_within");
}

fn sort_token_source(_token: i64, _sources: &[InsertSource]) -> Option<InsertSource> {
    todo!("port: sort_token_source");
}

fn sort_token_index(_token: i64, _indices: &[i64]) -> i64 {
    todo!("port: sort_token_index");
}

fn public_sort_value(_node: &OverlayNode, _token: i64, _sources: &[InsertSource], _indices: &[i64]) -> GetValue {
    todo!("port: public_sort_value");
}

fn restore_sort_override(
    _node: &OverlayNode,
    _token: i64,
    _sources: &[InsertSource],
    _indices: &[i64],
    _base_snapshot: &IndexMap<i64, Stored>,
    _insert_snapshots: &IndexMap<InsertSource, IndexMap<i64, Stored>>,
) {
    todo!("port: restore_sort_override");
}

fn same_sort_token_at(
    _overlay: &ArrayOverlay,
    _logical_index: i64,
    _token: i64,
    _sources: &[InsertSource],
    _indices: &[i64],
) -> bool {
    todo!("port: same_sort_token_at");
}

fn deduplicate_array_entries(_node: &OverlayNode) -> Result<()> {
    todo!("port: deduplicate_array_entries");
}

fn pieces_from_sort_order(_order: &[i64], _sources: &[InsertSource], _indices: &[i64]) -> Vec<Piece> {
    todo!("port: pieces_from_sort_order");
}

fn append_merged_piece(_pieces: &mut Vec<Piece>, _piece: Piece) {
    todo!("port: append_merged_piece");
}

fn singleton_piece(_piece: &Piece, _source_index: i64) -> Piece {
    todo!("port: singleton_piece");
}

fn clone_placement(_value: &Placement) -> Result<Stored> {
    todo!("port: clone_placement");
}

fn clone_node(_node: &OverlayNode) -> Result<Container> {
    todo!("port: clone_node");
}

fn clone_stored(_value: &Stored, _context: &OverlayContext) -> Result<Stored> {
    todo!("port: clone_stored");
}

fn clone_placement_node(_node: &OverlayNode) -> Result<Container> {
    todo!("port: clone_placement_node");
}

fn clone_placement_stored(_value: &Stored, _context: &OverlayContext) -> Result<Stored> {
    todo!("port: clone_placement_stored");
}

fn define_data(_target: &Container, _key: &str, _value: JsonValue) {
    todo!("port: define_data");
}

fn emit_operations(_context: &OverlayContext) -> Result<Vec<Op>> {
    todo!("port: emit_operations");
}

fn emit_simple_object_operations(_context: &OverlayContext) -> Result<Option<Vec<Op>>> {
    // Stable insertion sort avoids comparator/bucket allocations for the usual
    // handful of dirty object nodes.
    todo!("port: emit_simple_object_operations");
}

fn locate_dense_array_position(_node: &OverlayNode) -> bool {
    todo!("port: locate_dense_array_position");
}

fn has_covering_dense_region(
    _node: &OverlayNode,
    _path: &Path,
    _dense_regions: &IndexMap<OverlayNode, Vec<DenseRegion>>,
) -> bool {
    todo!("port: has_covering_dense_region");
}

fn add_dense_candidate(_candidates: &mut DenseCandidates, _index: i64) {
    todo!("port: add_dense_candidate");
}

fn record_dense_array_position(_node: &OverlayNode, _groups: &mut IndexMap<OverlayNode, DenseCandidates>) {
    todo!("port: record_dense_array_position");
}

fn build_dense_regions(_candidates: &DenseCandidates) -> Vec<DenseRegion> {
    todo!("port: build_dense_regions");
}

fn region_containing(_regions: &[DenseRegion], _index: i64) -> bool {
    todo!("port: region_containing");
}

fn has_reserved_mutation(_node: &OverlayNode) -> bool {
    todo!("port: has_reserved_mutation");
}

fn has_folded_ancestor(_node: &OverlayNode, _folded: &IndexSet<OverlayNode>) -> bool {
    todo!("port: has_folded_ancestor");
}

fn has_placement_ancestor(_node: &OverlayNode) -> bool {
    todo!("port: has_placement_ancestor");
}

fn emit_object_write(
    _node: &OverlayNode,
    _path: &Path,
    _operations: &mut Vec<Op>,
    _key: &str,
    _value: &Stored,
) -> Result<bool> {
    todo!("port: emit_object_write");
}

fn emit_object_operations(_node: &OverlayNode, _path: &Path, _operations: &mut Vec<Op>) -> Result<bool> {
    // Immutable replay must encode delete-and-readd explicitly so string-key
    // insertion order matches the draft.
    todo!("port: emit_object_operations");
}

fn build_array_plan(_node: &OverlayNode) -> ArrayPlan {
    todo!("port: build_array_plan");
}

fn range_length(_pieces: &[Piece], _start: i64, _end: i64) -> i64 {
    todo!("port: range_length");
}

fn emit_array_operations(
    _node: &OverlayNode,
    _path: &Path,
    _operations: &mut Vec<Op>,
    _dense_regions: Option<&[DenseRegion]>,
) -> Result<()> {
    todo!("port: emit_array_operations");
}

fn clone_array_region(_node: &OverlayNode, _region: &DenseRegion) -> Result<Vec<JsonValue>> {
    todo!("port: clone_array_region");
}

fn emit_changed_value(
    _operations: &mut Vec<Op>,
    _path: &NonEmptyPath,
    _before: Option<&JsonValue>,
    _after: &JsonValue,
) -> Result<bool> {
    // Slice/equality stays on V8's rope fast path; startsWith scans the entire
    // accumulated prefix on every append.
    todo!("port: emit_changed_value");
}

fn emit_set(_operations: &mut Vec<Op>, _path: &Path, _value: JsonValue) {
    todo!("port: emit_set");
}

fn resolve_path(_node: &OverlayNode) -> Option<Path> {
    todo!("port: resolve_path");
}

fn ensure_piece_locations(_overlay: &ArrayOverlay) {
    todo!("port: ensure_piece_locations");
}

fn find_entry_index(
    _overlay: &ArrayOverlay,
    _kind: ParentKind,
    _source_index: i64,
    _source: Option<&InsertSource>,
) -> Option<i64> {
    todo!("port: find_entry_index");
}

fn ensure_operations(_context: &OverlayContext) -> Result<Vec<Op>> {
    todo!("port: ensure_operations");
}

fn equal_trusted_json(_left: &JsonValue, _right: &JsonValue) -> bool {
    todo!("port: equal_trusted_json");
}

fn abort_context(_context: &OverlayContext) {
    todo!("port: abort_context");
}

fn release_overlay_references(_context: &OverlayContext) {
    todo!("port: release_overlay_references");
}

fn clear_context(_context: &OverlayContext) {
    todo!("port: clear_context");
}

// PORT: TS `unknown`. JSON uses `JsonValue`; an overlay proxy is `Placement::Node` / `GetValue::Node`.
fn is_container(_value: &JsonValue) -> bool {
    todo!("port: is_container");
}
