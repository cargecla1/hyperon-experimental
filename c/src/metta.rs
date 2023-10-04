use hyperon::common::shared::Shared;
use hyperon::space::DynSpace;
use hyperon::metta::text::*;
use hyperon::metta::interpreter;
use hyperon::metta::interpreter::InterpreterState;
use hyperon::metta::runner::{Metta, RunnerState};
use hyperon::metta::environment::{Environment, EnvBuilder};
use hyperon::rust_type_atom;

use crate::util::*;
use crate::atom::*;
use crate::space::*;

use core::borrow::Borrow;
use std::os::raw::*;
use regex::Regex;
use std::path::PathBuf;

// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-
// Tokenizer and Parser Interface
// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-

/// @brief Represents a handle to a Tokenizer, capable of recognizing meaningful Token substrings in text
/// @ingroup tokenizer_and_parser_group
/// @note `tokenizer_t` handles must be freed with `tokenizer_free()`
///
#[repr(C)]
pub struct tokenizer_t {
    /// Internal.  Should not be accessed directly
    tokenizer: *const RustTokenizer,
}

struct RustTokenizer(std::cell::RefCell<Tokenizer>);

impl From<Shared<Tokenizer>> for tokenizer_t {
    fn from(tokenizer: Shared<Tokenizer>) -> Self {
        Self{ tokenizer: std::rc::Rc::into_raw(tokenizer.0).cast() }
    }
}

impl tokenizer_t {
    fn borrow_inner(&self) -> &mut Tokenizer {
        let cell = unsafe{ &mut *(&(&*self.tokenizer).0 as *const std::cell::RefCell<Tokenizer>).cast_mut() };
        cell.get_mut()
    }
    fn clone_handle(&self) -> Shared<Tokenizer> {
        unsafe{ std::rc::Rc::increment_strong_count(self.tokenizer); }
        unsafe{ Shared(std::rc::Rc::from_raw(self.tokenizer.cast())) }
    }
    fn into_handle(self) -> Shared<Tokenizer> {
        unsafe{ Shared(std::rc::Rc::from_raw(self.tokenizer.cast())) }
    }
}

/// @brief Creates a new Tokenizer, without any registered Tokens
/// @ingroup tokenizer_and_parser_group
/// @return an `tokenizer_t` handle to access the newly created Tokenizer
/// @note The returned `tokenizer_t` handle must be freed with `tokenizer_free()`
///
#[no_mangle]
pub extern "C" fn tokenizer_new() -> tokenizer_t {
    Shared::new(Tokenizer::new()).into()
}

/// @brief Frees a Tokenizer handle
/// @ingroup tokenizer_and_parser_group
/// @param[in]  tokenizer  The `tokenizer_t` handle to free
/// @note When the last `tokenizer_t` handle for an underlying Tokenizer has been freed, then the
///    Tokenizer will be deallocated
///
#[no_mangle]
pub extern "C" fn tokenizer_free(tokenizer: tokenizer_t) {
    let tokenizer = tokenizer.into_handle();
    drop(tokenizer);
}

/// @struct token_api_t
/// @brief A table of callback functions to implement custom atom parsing
/// @ingroup tokenizer_and_parser_group
/// @see tokenizer_register_token
///
#[repr(C)]
pub struct token_api_t {

    /// @brief Creates a new Atom based the provided text
    /// @param[in]  str  A pointer to a C-style text string, that matched the associated regular expression
    /// @param[in]  context  A pointer to the `context` object supplied to `tokenizer_register_token()`
    /// @return An Atom created in response to the supplied text string
    ///
    construct_atom: extern "C" fn(str: *const c_char, context: *mut c_void) -> atom_t,

    /// @brief Frees the `context`, passed to `tokenizer_register_token()`, along with all other associated resources
    /// @param[in]  context  The pointer to the `context` to free
    /// @note Assigning NULL to this field means the context does not need to be freed
    ///
    free_context: Option<extern "C" fn(context: *mut c_void)>
}

//Internal wrapper to make sure the cleanup function gets called on the Token context
struct CToken {
    context: *mut c_void,
    api: *const token_api_t
}

impl Drop for CToken {
    fn drop(&mut self) {
        let free = unsafe{ (&*(*self).api).free_context };
        if let Some(free) = free {
            free(self.context);
        }
    }
}

/// @brief Registers a new custom Token in a Tokenizer
/// @ingroup tokenizer_and_parser_group
/// @param[in]  tokenizer  A pointer to the Tokenizer in which to register the Token
/// @param[in]  regex  A regular expression to match the incoming text, triggering this token to generate a new atom
/// @param[in]  api  A table of functions to manage the token
/// @param[in]  context  A caller-defined structure to communicate any state necessary to implement the Token parser
/// @note Hyperon uses the Rust RegEx engine and syntax, [documented here](https://docs.rs/regex/latest/regex/).
///
#[no_mangle]
pub extern "C" fn tokenizer_register_token(tokenizer: *mut tokenizer_t,
    regex: *const c_char, api: *const token_api_t, context: *mut c_void) {
    let tokenizer = unsafe{ &*tokenizer }.borrow_inner();
    let regex = Regex::new(cstr_as_str(regex)).unwrap();
    let c_token = CToken{ context, api };
    tokenizer.register_token(regex, move |token| {
        let constr = unsafe{ (&*c_token.api).construct_atom };
        let atom = constr(str_as_cstr(token).as_ptr(), c_token.context);
        atom.into_inner()
    });
}

/// @brief Performs a "deep copy" of a Tokenizer
/// @ingroup tokenizer_and_parser_group
/// @param[in]  tokenizer  A pointer to the Tokenizer to clone
/// @return The new Tokenizer, containing all registered Tokens belonging to the original Tokenizer
/// @note The returned `tokenizer_t` must be freed with `tokenizer_free()`
///
#[no_mangle]
pub extern "C" fn tokenizer_clone(tokenizer: *const tokenizer_t) -> tokenizer_t {
    let tokenizer = unsafe{ &*tokenizer }.borrow_inner();
    Shared::new(tokenizer.clone()).into()
}

/// @brief Represents an S-Expression Parser state machine, to parse input text into an Atom
/// @ingroup tokenizer_and_parser_group
/// @note `sexpr_parser_t` handles must be freed with `sexpr_parser_free()`
///
#[repr(C)]
pub struct sexpr_parser_t {
    /// Internal.  Should not be accessed directly
    parser: *const RustSExprParser,
    err_string: *mut c_char,
}

impl sexpr_parser_t {
    fn free_err_string(&mut self) {
        if !self.err_string.is_null() {
            let string = unsafe{ std::ffi::CString::from_raw(self.err_string) };
            drop(string);
            self.err_string = core::ptr::null_mut();
        }
    }
}

struct RustSExprParser(std::cell::RefCell<SExprParser<'static>>);

impl From<Shared<SExprParser<'static>>> for sexpr_parser_t {
    fn from(parser: Shared<SExprParser>) -> Self {
        Self{
            parser: std::rc::Rc::into_raw(parser.0).cast(),
            err_string: core::ptr::null_mut()
        }
    }
}

impl sexpr_parser_t {
    fn borrow_inner(&self) -> &mut SExprParser<'static> {
        let cell = unsafe{ &mut *(&(&*self.parser).0 as *const std::cell::RefCell<SExprParser>).cast_mut() };
        cell.get_mut()
    }
    fn into_handle(self) -> Shared<SExprParser<'static>> {
        unsafe{ Shared(std::rc::Rc::from_raw(self.parser.cast())) }
    }
}

/// @brief Creates a new S-Expression Parser
/// @ingroup tokenizer_and_parser_group
/// @param[in]  text  A C-style string containing the input text to parse
/// @return The new `sexpr_parser_t`, ready to parse the text
/// @note The returned `sexpr_parser_t` must be freed with `sexpr_parser_free()`
/// @warning The returned `sexpr_parser_t` borrows a reference to the `text` pointer, so the returned
///    `sexpr_parser_t` must be freed before the `text` is freed or allowed to go out of scope.
///
#[no_mangle]
pub extern "C" fn sexpr_parser_new(text: *const c_char) -> sexpr_parser_t {
    Shared::new(SExprParser::new(cstr_as_str(text))).into()
}

/// @brief Frees an S-Expression Parser
/// @ingroup tokenizer_and_parser_group
/// @param[in]  parser  The `sexpr_parser_t` handle to free
///
#[no_mangle]
pub extern "C" fn sexpr_parser_free(parser: sexpr_parser_t) {
    let parser = parser.into_handle();
    drop(parser);
}

/// @brief Parses the text associated with an `sexpr_parser_t`, and creates the corresponding Atom
/// @ingroup tokenizer_and_parser_group
/// @param[in]  parser  A pointer to the Parser, which is associated with the text to parse
/// @param[in]  tokenizer  A pointer to the Tokenizer, to use to interpret atoms within the expression
/// @return The new `atom_t`, which may be an Expression atom with many child atoms
/// @note The caller must take ownership responsibility for the returned `atom_t`, and ultimately free
///   it with `atom_free()` or pass it to another function that takes ownership responsibility
///
#[no_mangle]
pub extern "C" fn sexpr_parser_parse(
    parser: *mut sexpr_parser_t,
    tokenizer: *const tokenizer_t) -> atom_t
{
    let parser = unsafe{ &mut *parser };
    parser.free_err_string();
    let rust_parser = parser.borrow_inner();
    let tokenizer = unsafe{ &*tokenizer }.borrow_inner();
    match rust_parser.parse(tokenizer) {
        Ok(atom) => atom.into(),
        Err(err) => {
            let err_cstring = std::ffi::CString::new(err).unwrap();
            parser.err_string = err_cstring.into_raw();
            atom_t::null()
        }
    }
}

/// @brief Returns the error string associated with the last `sexpr_parser_parse` call
/// @ingroup tokenizer_and_parser_group
/// @param[in]  parser  A pointer to the Parser, which is associated with the text to parse
/// @return A pointer to the C-string containing the parse error that occurred, or NULL if no
///     parse error occurred
/// @warning The returned pointer should NOT be freed.  It must never be accessed after the
///     sexpr_parser_t has been freed, or any subsequent `sexpr_parser_parse` or
///     `sexpr_parser_parse_to_syntax_tree` has been made.
///
#[no_mangle]
pub extern "C" fn sexpr_parser_err_str(
    parser: *mut sexpr_parser_t) -> *const c_char {
    let parser = unsafe{ &*parser };
    parser.err_string
}

/// @brief Represents a component in a syntax tree created by parsing MeTTa code
/// @ingroup tokenizer_and_parser_group
/// @note `syntax_node_t` objects must be freed with `syntax_node_free()`
///
#[repr(C)]
pub struct syntax_node_t {
    /// Internal.  Should not be accessed directly
    node: *mut RustSyntaxNode,
}

struct RustSyntaxNode(SyntaxNode);

impl From<SyntaxNode> for syntax_node_t {
    fn from(node: SyntaxNode) -> Self {
        Self{ node: Box::into_raw(Box::new(RustSyntaxNode(node))) }
    }
}

impl From<Option<SyntaxNode>> for syntax_node_t {
    fn from(node: Option<SyntaxNode>) -> Self {
        match node {
            Some(node) => Self{ node: Box::into_raw(Box::new(RustSyntaxNode(node))) },
            None => syntax_node_t::null()
        }
    }
}

impl syntax_node_t {
    fn into_inner(self) -> SyntaxNode {
        unsafe{ (*Box::from_raw(self.node)).0 }
    }
    fn borrow(&self) -> &SyntaxNode {
        &unsafe{ &*(&*self).node }.0
    }
    fn is_null(&self) -> bool {
        self.node == core::ptr::null_mut()
    }
    fn null() -> Self {
        Self{node: core::ptr::null_mut()}
    }
}

/// @brief The type of language construct respresented by a syntax_node_t
/// @ingroup tokenizer_and_parser_group
///
#[repr(C)]
pub enum syntax_node_type_t {
    /// @brief A Comment, beginning with a ';' character
    COMMENT,
    /// @brief A variable.  A symbol immediately preceded by a '$' sigil
    VARIABLE_TOKEN,
    /// @brief A String Literal.  All text between non-escaped '"' (double quote) characters
    STRING_TOKEN,
    /// @brief Word Token.  Any other whitespace-delimited token that isn't a VARIABLE_TOKEN or STRING_TOKEN
    WORD_TOKEN,
    /// @brief Open Parenthesis.  A non-escaped '(' character indicating the beginning of an expression
    OPEN_PAREN,
    /// @brief Close Parenthesis.  A non-escaped ')' character indicating the end of an expression
    CLOSE_PAREN,
    /// @brief Whitespace. One or more whitespace chars
    WHITESPACE,
    /// @brief Leftover Text that remains unparsed after a parse error has occurred
    LEFTOVER_TEXT,
    /// @brief A Group of nodes between an `OPEN_PAREN` and a matching `CLOSE_PAREN`
    EXPRESSION_GROUP,
    /// @brief A Group of nodes that cannot be combined into a coherent atom due to a parse error,
    ///     even if some of the individual nodes could represent valid atoms
    ERROR_GROUP,
}

impl From<SyntaxNodeType> for syntax_node_type_t {
    fn from(node_type: SyntaxNodeType) -> Self {
        match node_type {
            SyntaxNodeType::Comment => Self::COMMENT,
            SyntaxNodeType::VariableToken => Self::VARIABLE_TOKEN,
            SyntaxNodeType::StringToken => Self::STRING_TOKEN,
            SyntaxNodeType::WordToken => Self::WORD_TOKEN,
            SyntaxNodeType::OpenParen => Self::OPEN_PAREN,
            SyntaxNodeType::CloseParen => Self::CLOSE_PAREN,
            SyntaxNodeType::Whitespace => Self::WHITESPACE,
            SyntaxNodeType::LeftoverText => Self::LEFTOVER_TEXT,
            SyntaxNodeType::ExpressionGroup => Self::EXPRESSION_GROUP,
            SyntaxNodeType::ErrorGroup => Self::ERROR_GROUP,
        }
    }
}

/// @brief Function signature for a callback providing access to a `syntax_node_t`
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  The `syntax_node_t` being provided.  This node should not be modified or freed by the callback.
/// @param[in]  context  The context state pointer initially passed to the upstream function initiating the callback.
///
pub type c_syntax_node_callback_t = extern "C" fn(node: *const syntax_node_t, context: *mut c_void);

/// @brief Parses the text associated with an `sexpr_parser_t`, and creates a syntax tree
/// @ingroup tokenizer_and_parser_group
/// @param[in]  parser  A pointer to the Parser, which is associated with the text to parse
/// @return The new `syntax_node_t` representing the root of the parsed tree
/// @note The caller must take ownership responsibility for the returned `syntax_node_t`, and ultimately free
///   it with `syntax_node_free()`
///
#[no_mangle]
pub extern "C" fn sexpr_parser_parse_to_syntax_tree(parser: *mut sexpr_parser_t) -> syntax_node_t
{
    let parser = unsafe{ &mut *parser };
    parser.free_err_string();
    let rust_parser = parser.borrow_inner();
    rust_parser.parse_to_syntax_tree().into()
}

/// @brief Frees a syntax_node_t
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  The `syntax_node_t` to free
///
#[no_mangle]
pub extern "C" fn syntax_node_free(node: syntax_node_t) {
    let node = node.into_inner();
    drop(node);
}

/// @brief Creates a deep copy of a `syntax_node_t`
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  A pointer to the `syntax_node_t`
/// @return The `syntax_node_t` representing the cloned syntax node
/// @note The caller must take ownership responsibility for the returned `syntax_node_t`, and ultimately free
///   it with `syntax_node_free()`
///
#[no_mangle]
pub extern "C" fn syntax_node_clone(node: *const syntax_node_t) -> syntax_node_t {
    let node = unsafe{ &*node }.borrow();
    node.clone().into()
}

/// @brief Performs a depth-first iteration of all child syntax nodes within a syntax tree
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  A pointer to the top-level `syntax_node_t` representing the syntax tree
/// @param[in]  callback  A function that will be called to provide a vector of all type atoms associated with the `atom` argument atom
/// @param[in]  context  A pointer to a caller-defined structure to facilitate communication with the `callback` function
///
#[no_mangle]
pub extern "C" fn syntax_node_iterate(node: *const syntax_node_t,
    callback: c_syntax_node_callback_t, context: *mut c_void) {
    let node = unsafe{ &*node }.borrow();
    node.visit_depth_first(|node| {
        let node = syntax_node_t{node: (node as *const SyntaxNode).cast_mut().cast()};
        callback(&node, context);
    });
}

/// @brief Returns the type of a `syntax_node_t`
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  A pointer to the `syntax_node_t`
/// @return The `syntax_node_type_t` representing the type of the syntax node
///
#[no_mangle]
pub extern "C" fn syntax_node_type(node: *const syntax_node_t) -> syntax_node_type_t {
    let node = unsafe{ &*node }.borrow();
    node.node_type.into()
}

/// @brief Returns `true` if a syntax node represents the end of the stream
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  A pointer to the `syntax_node_t`
/// @return The boolean value indicating if the node is a a null node
///
#[no_mangle]
pub extern "C" fn syntax_node_is_null(node: *const syntax_node_t) -> bool {
    unsafe{ &*node }.is_null()
}

/// @brief Returns `true` if a syntax node is a leaf (has no children) and `false` otherwise
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  A pointer to the `syntax_node_t`
/// @return The boolean value indicating if the node is a leaf
///
#[no_mangle]
pub extern "C" fn syntax_node_is_leaf(node: *const syntax_node_t) -> bool {
    let node = unsafe{ &*node }.borrow();
    node.node_type.is_leaf()
}

/// @brief Returns the beginning and end positions in the parsed source of the text represented by the syntax node
/// @ingroup tokenizer_and_parser_group
/// @param[in]  node  A pointer to the `syntax_node_t`
/// @param[out]  range_start  A pointer to a value, into which the starting offset of the range will be written
/// @param[out]  range_end  A pointer to a value, into which the ending offset of the range will be written
///
#[no_mangle]
pub extern "C" fn syntax_node_src_range(node: *const syntax_node_t, range_start: *mut usize, range_end: *mut usize) {
    let node = unsafe{ &*node }.borrow();
    unsafe{ *range_start = node.src_range.start; }
    unsafe{ *range_end = node.src_range.end; }
}

// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-
// MeTTa Language and Types
// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-

//TODO After Alpha.  These MeTTa keyword atoms should be static objects rather than functions, once I make
// atom_t fully repr(C), then I will change this, so the caller doesn't need to worry about freeing the
// return values

/// @brief Checks if an atom is a MeTTa error expression
/// @ingroup metta_language_group
/// @param[in]  atom  A pointer to an `atom_t` or an `atom_ref_t` representing the atom to check
/// @return `true` is the atom is a MeTTa error expression
///
#[no_mangle]
pub extern "C" fn atom_is_error(atom: *const atom_ref_t) -> bool {
    let atom = unsafe{ &*atom }.borrow();
    hyperon::metta::runner::atom_is_error(atom)
}

/// @brief Creates a Symbol atom for the special MeTTa symbol: "%Undefined%"
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_UNDEFINED() -> atom_t { hyperon::metta::ATOM_TYPE_UNDEFINED.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol: "Type", used to indicate that an atom represents the type of another atom
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_TYPE() -> atom_t { hyperon::metta::ATOM_TYPE_TYPE.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol: "Atom", used to indicate that an atom's type is a generic atom
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_ATOM() -> atom_t { hyperon::metta::ATOM_TYPE_ATOM.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol: "Symbol", used to indicate that an atom's type is a symbol atom
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_SYMBOL() -> atom_t { hyperon::metta::ATOM_TYPE_SYMBOL.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol: "Variable", used to indicate that an atom's type is a variable atom
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_VARIABLE() -> atom_t { hyperon::metta::ATOM_TYPE_VARIABLE.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol: "Expression", used to indicate that an atom's type is an expression atom
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_EXPRESSION() -> atom_t { hyperon::metta::ATOM_TYPE_EXPRESSION.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol: "Grounded", used to indicate that an atom's type is a grounded atom
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_GROUNDED() -> atom_t { hyperon::metta::ATOM_TYPE_GROUNDED.into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol used to indicate that an atom's type is a wrapper around a Space
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Symbol atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn ATOM_TYPE_GROUNDED_SPACE() -> atom_t { rust_type_atom::<DynSpace>().into() }

/// @brief Creates a Symbol atom for the special MeTTa symbol used to indicate empty results in
/// case expressions.
/// @ingroup metta_language_group
/// @return  The `atom_t` representing the Void atom
/// @note The returned `atom_t` must be freed with `atom_free()`
///
#[no_mangle] pub extern "C" fn VOID_SYMBOL() -> atom_t { hyperon::metta::VOID_SYMBOL.into() }

/// @brief Checks whether Atom `atom` has Type `typ` in context of `space`
/// @ingroup metta_language_group
/// @param[in]  space  A pointer to the `space_t` representing the space context in which to perform the check
/// @param[in]  atom  A pointer to the `atom_t` or `atom_ref_t` representing the atom whose Type the function will check
/// @param[in]  typ  A pointer to the `atom_t` or `atom_ref_t` representing the type to check against
/// @return  `true` if the Atom's Type is a match, otherwise `false`
/// @note This function can be used for a simple type check when there is no need to know type parameters
///
#[no_mangle]
pub extern "C" fn check_type(space: *const space_t, atom: *const atom_ref_t, typ: *const atom_ref_t) -> bool {
    let dyn_space = unsafe{ &*space }.borrow();
    let atom = unsafe{ &*atom }.borrow();
    let typ = unsafe{ &*typ }.borrow();
    hyperon::metta::types::check_type(dyn_space.borrow().as_space(), atom, typ)
}

/// @brief Checks whether `atom` is correctly typed
/// @ingroup metta_language_group
/// @param[in]  space  A pointer to the `space_t` representing the space context in which to perform the check
/// @param[in]  atom  A pointer to the `atom_t` or `atom_ref_t` representing the atom whose Type the function will check
/// @return  `true` if the Atom is correctly typed, otherwise `false`
/// @note This function can be used to check if function arguments have correct types
///
#[no_mangle]
pub extern "C" fn validate_atom(space: *const space_t, atom: *const atom_ref_t) -> bool {
    let dyn_space = unsafe{ &*space }.borrow();
    let atom = unsafe{ &*atom }.borrow();
    hyperon::metta::types::validate_atom(dyn_space.borrow().as_space(), atom)
}

/// @brief Provides all types for `atom` in the context of `space`
/// @ingroup metta_language_group
/// @param[in]  space  A pointer to the `space_t` representing the space context in which to access the Atom's types
/// @param[in]  atom  A pointer to the `atom_t` or `atom_ref_t` representing the atom whose Types the function will access
/// @param[in]  callback  A function that will be called to provide a vector of all type atoms associated with the `atom` argument atom
/// @param[in]  context  A pointer to a caller-defined structure to facilitate communication with the `callback` function
/// @note `%Undefined%` will be provided if `atom` has no type assigned. An empty vector will be provided if `atom`
///    is a function call but expected types of arguments are not compatible with passed values
///
#[no_mangle]
pub extern "C" fn get_atom_types(space: *const space_t, atom: *const atom_ref_t,
        callback: c_atom_vec_callback_t, context: *mut c_void) {
    let dyn_space = unsafe{ &*space }.borrow();
    let atom = unsafe{ (&*atom).borrow() };
    let types = hyperon::metta::types::get_atom_types(dyn_space.borrow().as_space(), atom);
    return_atoms(&types, callback, context);
}

// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-
// MeTTa Intperpreter Interface
// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-

//QUESTION: It feels like a runner_state_t and a step_result_t are getting at the same functionality,
// but at different levels.  I think it probably makes sense to remove step_result_t from the C
// and Python APIs to cut down on API surface area

/// @brief Contains the state for an in-flight interpreter operation
/// @ingroup interpreter_group
/// @note A `step_result_t` is initially created by `interpret_init()`.  Each call to `interpret_step()`, in
///    a loop, consumes the `step_result_t` and creates a new one.  When the interpreter operation has
///    fully resolved, `step_get_result()` can provide the final results.  Ownership of the `step_result_t`
///    must ultimately be released with `step_get_result()`.
/// @see interpret_init
/// @see interpret_step
/// @see step_get_result
///
#[repr(C)]
pub struct step_result_t {
    /// Internal.  Should not be accessed directly
    result: *mut RustStepResult,
}

struct RustStepResult(InterpreterState<'static, DynSpace>);

impl From<InterpreterState<'static, DynSpace>> for step_result_t {
    fn from(state: InterpreterState<'static, DynSpace>) -> Self {
        Self{ result: Box::into_raw(Box::new(RustStepResult(state))) }
    }
}

impl step_result_t {
    fn into_inner(self) -> InterpreterState<'static, DynSpace> {
        unsafe{ Box::from_raw(self.result).0 }
    }
    fn borrow(&self) -> &InterpreterState<'static, DynSpace> {
        &unsafe{ &*(&*self).result }.0
    }
}

/// @brief Initializes an interpreter operation and take the initial step
/// @ingroup interpreter_group
/// @param[in]  space  A pointer to the Space in which to perform the operation
/// @param[in]  expr  A pointer to an `atom_t` or `atom_ref_t` Expression atom to interpret
/// @return A `step_result_t` representing the outcome from the initial step
/// @note The returned value may represent an error, an immediate result value, or it may be necessary to
///    call `interpret_step()` in a loop to fully evaluate the execution plan.  Ultimately `step_get_result()`
///    must be called to release the returned `step_result_t`
///
#[no_mangle]
pub extern "C" fn interpret_init(space: *mut space_t, expr: *const atom_ref_t) -> step_result_t {
    let dyn_space = unsafe{ &*space }.borrow();
    let expr = unsafe{ (&*expr).borrow() };
    let step = interpreter::interpret_init(dyn_space.clone(), expr);
    step.into()
}

/// @brief Takes a subsequent step in an in-flight interpreter operation
/// @ingroup interpreter_group
/// @param[in]  step  The existing state for the in-flight interpreter operation
/// @return A new `step_result_t` representing the outcome from the step
///
#[no_mangle]
pub extern "C" fn interpret_step(step: step_result_t) -> step_result_t {
    let step = step.into_inner();
    let next = interpreter::interpret_step(step);
    next.into()
}

/// @brief Renders a text description of a `step_result_t` into a buffer
/// @ingroup interpreter_group
/// @param[in]  step  A pointer to a `step_result_t` to render
/// @param[out]  buf  A buffer into which the text will be rendered
/// @param[in]  buf_len  The maximum allocated size of `buf`
/// @return The length of the description string, minus the string terminator character.  If
///    `return_value > buf_len + 1`, then the text was not fully rendered and this function should be
///    called again with a larger buffer.
///
#[no_mangle]
pub extern "C" fn step_to_str(step: *const step_result_t, buf: *mut c_char, buf_len: usize) -> usize {
    let step = unsafe{ &*step }.borrow();
    write_debug_into_buf(step, buf, buf_len)
}

/// @brief Examines a `step_result_t` to determine if more work is needed
/// @ingroup interpreter_group
/// @param[in]  step  A pointer to the `step_result_t` representing the in-flight interpreter operation
/// @return `true` if the operation plan indicates that more work is needed to finalize results, otherwise `false`
///
#[no_mangle]
pub extern "C" fn step_has_next(step: *const step_result_t) -> bool {
    let step = unsafe{ &*step }.borrow();
    step.has_next()
}

/// @brief Consumes a `step_result_t` and provides the ultimate outcome of a MeTTa interpreter session 
/// @ingroup interpreter_group
/// @param[in]  step  A pointer to a `step_result_t` to render
/// @param[in]  callback  A function that will be called to provide a vector of all atoms resulting from the interpreter session
/// @param[in]  context  A pointer to a caller-defined structure to facilitate communication with the `callback` function
///
#[no_mangle]
pub extern "C" fn step_get_result(step: step_result_t,
        callback: c_atom_vec_callback_t, context: *mut c_void) {
    let step = step.into_inner();
    match step.into_result() {
        Ok(res) => return_atoms(&res, callback, context),
        Err(_) => return_atoms(&vec![], callback, context),
    }
}

/// @brief A top-level MeTTa Interpreter
/// @ingroup interpreter_group
/// @note A `metta_t` must be freed with `metta_free()`
/// @see metta_new
/// @see metta_free
///
#[repr(C)]
pub struct metta_t {
    /// Internal.  Should not be accessed directly
    metta: *mut RustMettaInterpreter,
}

struct RustMettaInterpreter(Metta);

impl From<Metta> for metta_t {
    fn from(metta: Metta) -> Self {
        Self{ metta: Box::into_raw(Box::new(RustMettaInterpreter(metta))) }
    }
}

impl metta_t {
    fn borrow(&self) -> &Metta {
        unsafe{ &(&*self.metta).0  }
    }
    fn into_inner(self) -> Metta {
        unsafe{ Box::from_raw(self.metta).0 }
    }
}

/// @brief Creates a new top-level MeTTa Interpreter, with only the Rust stdlib loaded
/// @ingroup interpreter_group
/// @return A `metta_t` handle to the newly created Interpreter
/// @note The caller must take ownership responsibility for the returned `metta_t`, and free it with `metta_free()`
///
#[no_mangle]
pub extern "C" fn metta_new_rust() -> metta_t {
    let metta = Metta::new_rust();
    metta.into()
}

/// @brief Creates a new MeTTa Interpreter with a provided Space and Tokenizer
/// @ingroup interpreter_group
/// @param[in]  space  A pointer to a handle for the Space for use by the Interpreter
/// @param[in]  tokenizer  A pointer to a handle for the Tokenizer for use by the Interpreter
/// @return A `metta_t` handle to the newly created Interpreter
/// @note The caller must take ownership responsibility for the returned `metta_t`, and free it with `metta_free()`
/// @note This function does not load any stdlib, nor does it run the `init.metta` file from the environment
///
#[no_mangle]
pub extern "C" fn metta_new_with_space(space: *mut space_t, tokenizer: *mut tokenizer_t) -> metta_t {
    let dyn_space = unsafe{ &*space }.borrow();
    let tokenizer = unsafe{ &*tokenizer }.clone_handle();
    let metta = Metta::new_with_space(dyn_space.clone(), tokenizer);
    metta.into()
}

/// @brief Frees a `metta_t` handle
/// @ingroup interpreter_group
/// @param[in]  metta  The handle to free
/// @note The underlying Interpreter may be deallocated if all handles that refer to it have been freed, otherwise
///    the Interpreter itself won't be freed
///
#[no_mangle]
pub extern "C" fn metta_free(metta: metta_t) {
    let metta = metta.into_inner();
    drop(metta);
}

/// @brief Initializes a MeTTa interpreter, for manually bootstrapping a multi-language interpreter
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @note Most callers can simply call `metta_new_rust`.  This function is provided to support languages
///     with their own stdlib, that needs to be loaded before the init.metta file is run
///
#[no_mangle]
pub extern "C" fn metta_init_with_platform_env(metta: *mut metta_t) {
    let metta = unsafe{ &*metta }.borrow();
    metta.init_with_platform_env()
}

/// @brief Provides access to the Space associated with a MeTTa Interpreter
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @return A Space handle, to access the Space associated with the Interpreter
/// @note The caller must take ownership responsibility for the returned `space_t` and free it with `space_free()`
///
#[no_mangle]
pub extern "C" fn metta_space(metta: *mut metta_t) -> space_t {
    let metta = unsafe{ &*metta }.borrow();
    metta.space().clone().into()
}

/// @brief Provides access to the Tokenizer associated with a MeTTa Interpreter
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @return A Tokenizer handle, to access the Tokenizer associated with the Interpreter
/// @note The caller must take ownership responsibility for the returned `tokenizer_t` and free it with `tokenizer_free()`
///
#[no_mangle]
pub extern "C" fn metta_tokenizer(metta: *mut metta_t) -> tokenizer_t {
    let metta = unsafe{ &*metta }.borrow();
    metta.tokenizer().clone().into()
}

/// @brief Runs the MeTTa Interpreter until the input text has been parsed and evaluated
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @param[in]  parser  A pointer to the S-Expression Parser handle, containing the expression text
/// @param[in]  callback  A function that will be called to provide a vector of atoms produced by the evaluation
/// @param[in]  context  A pointer to a caller-defined structure to facilitate communication with the `callback` function
///
#[no_mangle]
pub extern "C" fn metta_run(metta: *mut metta_t, parser: *mut sexpr_parser_t,
        callback: c_atom_vec_callback_t, context: *mut c_void) {
    let metta = unsafe{ &*metta }.borrow();
    let mut parser = unsafe{ &*parser }.borrow_inner();
    let results = metta.run(&mut parser);
    // TODO: return erorrs properly after step_get_result() is changed to return errors.
    for result in results.expect("Returning errors from C API is not implemented yet") {
        return_atoms(&result, callback, context);
    }
}

/// @brief Represents the state of a MeTTa runner
/// @ingroup interpreter_group
/// @note `runner_state_t` handles must be freed with `runner_state_free()`
///
#[repr(C)]
pub struct runner_state_t {
    /// Internal.  Should not be accessed directly
    state: *mut RustRunnerState,
}

struct RustRunnerState(RunnerState<'static>);

impl From<RunnerState<'static>> for runner_state_t {
    fn from(state: RunnerState<'static>) -> Self {
        Self{ state: Box::into_raw(Box::new(RustRunnerState(state))) }
    }
}

impl runner_state_t {
    fn into_inner(self) -> RunnerState<'static> {
        unsafe{ Box::from_raw(self.state).0 }
    }
    fn borrow(&self) -> &RunnerState<'static> {
        &unsafe{ &*(&*self).state }.0
    }
    fn borrow_mut(&mut self) -> &mut RunnerState<'static> {
        &mut unsafe{ &mut *(&*self).state }.0
    }
}

/// @brief Creates a runner_state_t, to use for step-wise execution
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @return The newly created `runner_state_t`, which can begin evaluating MeTTa code
/// @note The returned `runner_state_t` handle must be freed with `runner_state_free()`
///
#[no_mangle]
pub extern "C" fn metta_start_run(metta: *mut metta_t) -> runner_state_t {
    let metta = unsafe{ &*metta }.borrow();
    let state = metta.start_run();
    state.into()
}

/// @brief Frees a runner_state_t
/// @ingroup interpreter_group
/// @param[in]  node  The `runner_state_t` to free
///
#[no_mangle]
pub extern "C" fn runner_state_free(state: runner_state_t) {
    let state = state.into_inner();
    drop(state);
}

/// @brief Runs one step of the interpreter
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @param[in]  parser  A pointer to the S-Expression Parser handle, containing the expression text
/// @param[in]  state  A pointer to the in-flight runner state
///
#[no_mangle]
pub extern "C" fn metta_run_step(metta: *mut metta_t, parser: *mut sexpr_parser_t, state: *mut runner_state_t) {
    let metta = unsafe{ &*metta }.borrow();
    let parser = unsafe{ &*parser }.borrow_inner();
    let state = unsafe{ &mut *state }.borrow_mut();
    metta.run_step(parser, state).unwrap_or_else(|err| panic!("Unhandled MeTTa error: {}", err));
}

/// @brief Returns whether or not the runner_state_t has completed all outstanding work
/// @ingroup interpreter_group
/// @param[in]  state  The `runner_state_t` to inspect
/// @return `true` if the runner has already concluded, or `false` if there is more work to do
///
#[no_mangle]
pub extern "C" fn runner_state_is_complete(state: *const runner_state_t) -> bool {
    let state = unsafe{ &*state }.borrow();
    state.is_complete()
}

/// @brief Accesses the current in-flight results in the runner_state_t
/// @ingroup interpreter_group
/// @param[in]  state  The `runner_state_t` within which to preview results
/// @param[in]  callback  A function that will be called to provide a vector of atoms produced by the evaluation
/// @param[in]  context  A pointer to a caller-defined structure to facilitate communication with the `callback` function
/// @warning The provided results will be overwritten by the next call to `metta_run_step`, so the caller
///     must clone the result atoms if they are needed for an extended period of time
///
#[no_mangle]
pub extern "C" fn runner_state_current_results(state: *const runner_state_t,
        callback: c_atom_vec_callback_t, context: *mut c_void) {
    let state = unsafe{ &*state }.borrow();
    let results = state.current_results();
    for result in results {
        return_atoms(result, callback, context);
    }
}

/// @brief Runs the MeTTa Interpreter to evaluate an input Atom
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the Interpreter handle
/// @param[in]  atom  The `atom_t` representing the atom to evaluate
/// @param[in]  callback  A function that will be called to provide a vector of atoms produced by the evaluation
/// @param[in]  context  A pointer to a caller-defined structure to facilitate communication with the `callback` function
/// @warning This function takes ownership of the provided `atom_t`, so it must not be subsequently accessed or freed
///
#[no_mangle]
pub extern "C" fn metta_evaluate_atom(metta: *mut metta_t, atom: atom_t,
        callback: c_atom_vec_callback_t, context: *mut c_void) {
    let metta = unsafe{ &*metta }.borrow();
    let atom = atom.into_inner();
    let result = metta.evaluate_atom(atom)
        .expect("Returning errors from C API is not implemented yet");
    return_atoms(&result, callback, context);
}

/// @brief Loads a module into a MeTTa interpreter
/// @ingroup interpreter_group
/// @param[in]  metta  A pointer to the handle specifying the interpreter into which to load the module
/// @param[in]  name  A C-style string containing the module name
///
#[no_mangle]
pub extern "C" fn metta_load_module(metta: *mut metta_t, name: *const c_char) {
    let metta = unsafe{ &*metta }.borrow();
    // TODO: return erorrs properly
    metta.load_module(PathBuf::from(cstr_as_str(name)))
        .expect("Returning errors from C API is not implemented yet");
}

// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-
// Environment Interface
// =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-

/// @brief Renders the config_dir path from the platform environment into a text buffer
/// @ingroup environment_group
/// @param[out]  buf  A buffer into which the text will be written
/// @param[in]  buf_len  The maximum allocated size of `buf`
/// @return The length of the path string, minus the string terminator character.  If
/// `return_value > buf_len + 1`, then the text was not fully written and this function should be
/// called again with a larger buffer.  This function will return 0 if there is no config_dir.
///
#[no_mangle]
pub extern "C" fn environment_config_dir(buf: *mut c_char, buf_len: usize) -> usize {
    match Environment::platform_env().config_dir() {
        Some(path) => write_into_buf(path.display(), buf, buf_len),
        None => 0
    }
}

/// @brief Returns the number of module search paths in the environment
/// @ingroup environment_group
/// @return The number of paths in the Environment
///
#[no_mangle]
pub extern "C" fn environment_search_path_cnt() -> usize {
    Environment::platform_env().modules_search_paths().count()
}

/// @brief Renders nth module search path from the platform environment into a text buffer
/// @ingroup environment_group
/// @param[in]  idx  The index of the search path to render, with 0 being the highest priority search
///     path, and subsequent indices having decreasing search priority.
///     If `i > environment_search_path_cnt()`, this function will return 0.
/// @param[out]  buf  A buffer into which the text will be written
/// @param[in]  buf_len  The maximum allocated size of `buf`
/// @return The length of the path string, minus the string terminator character.  If
///     `return_value > buf_len + 1`, then the text was not fully written and this function should be
///     called again with a larger buffer.  This function will return 0 if the input arguments don't
///     specify a valid search path.
///
#[no_mangle]
pub extern "C" fn environment_nth_search_path(idx: usize, buf: *mut c_char, buf_len: usize) -> usize {
    let path = Environment::platform_env().modules_search_paths().nth(idx);
    match path {
        Some(path) => write_into_buf(path.display(), buf, buf_len),
        None => 0
    }
}

/// @brief Represents the environment initialization, in progress
/// @ingroup environment_group
/// @note `env_builder_t` must be given to `environment_init_finish()` to properly release it
///
#[repr(C)]
pub struct env_builder_t {
    /// Internal.  Should not be accessed directly
    builder: *mut RustEnvBuilder,
}

struct RustEnvBuilder(EnvBuilder);

impl From<EnvBuilder> for env_builder_t {
    fn from(builder: EnvBuilder) -> Self {
        Self{ builder: Box::into_raw(Box::new(RustEnvBuilder(builder))) }
    }
}

impl env_builder_t {
    fn into_inner(self) -> EnvBuilder {
        unsafe{ Box::from_raw(self.builder).0 }
    }
    fn null() -> Self {
        Self {builder: core::ptr::null_mut()}
    }
}

/// @brief Begins the initialization of the platform environment
/// @ingroup environment_group
/// @return The `env_builder_t` object representing the in-process environment initialization
/// @note environment_init_finish must be called after environment initialization is finished
///
#[no_mangle]
pub extern "C" fn environment_init_start() -> env_builder_t {
    EnvBuilder::new().into()
}

/// @brief Finishes initialization of the platform environment
/// @ingroup environment_group
/// @param[in]  builder  The in-process environment builder state to install as the platform environment
/// @return  True if the environment was sucessfully initialized with the provided builder state.  False
///     if the environment had already been initialized by a prior call
///
#[no_mangle]
pub extern "C" fn environment_init_finish(builder: env_builder_t) -> bool {
    let builder = builder.into_inner();
    builder.try_init_platform_env().is_ok()
}

/// @brief Sets the working directory for the platform environment
/// @ingroup environment_group
/// @param[in]  builder  A pointer to the in-process environment builder state
/// @param[in]  path  A C-style string specifying a path to a working directory, to search for modules to load.
///     Passing `NULL` will unset the working directory
/// @note This working directory is not required to be the same as the process working directory, and
///   it will not change as the process' working directory is changed
///
#[no_mangle]
pub extern "C" fn environment_init_set_working_dir(builder: *mut env_builder_t, path: *const c_char) {
    let builder_arg_ref = unsafe{ &mut *builder };
    let builder = core::mem::replace(builder_arg_ref, env_builder_t::null()).into_inner();
    let builder = if path.is_null() {
        builder.set_working_dir(None)
    } else {
        builder.set_working_dir(Some(&PathBuf::from(cstr_as_str(path))))
    };
    *builder_arg_ref = builder.into();
}

/// @brief Sets the config directory for the platform environment.  A directory at the specified path
/// will be created, and its contents populated with default values, if one does not already exist
/// @ingroup environment_group
/// @param[in]  builder  A pointer to the in-process environment builder state
/// @param[in]  path  A C-style string specifying a path to a working directory, to search for modules to load
///
#[no_mangle]
pub extern "C" fn environment_init_set_config_dir(builder: *mut env_builder_t, path: *const c_char) {
    let builder_arg_ref = unsafe{ &mut *builder };
    let builder = core::mem::replace(builder_arg_ref, env_builder_t::null()).into_inner();
    let builder = if path.is_null() {
        panic!("Fatal Error: path cannot be NULL");
    } else {
        builder.set_config_dir(&PathBuf::from(cstr_as_str(path)))
    };
    *builder_arg_ref = builder.into();
}

/// @brief Configures the platform environment so that no config directory will be read nor created
/// @ingroup environment_group
/// @param[in]  builder  A pointer to the in-process environment builder state
///
#[no_mangle]
pub extern "C" fn environment_init_disable_config_dir(builder: *mut env_builder_t) {
    let builder_arg_ref = unsafe{ &mut *builder };
    let builder = core::mem::replace(builder_arg_ref, env_builder_t::null()).into_inner();
    let builder = builder.set_no_config_dir();
    *builder_arg_ref = builder.into();
}

/// @brief Adds a config directory to search for imports.  The most recently added paths will be searched
///     first, continuing in inverse order
/// @ingroup environment_group
/// @param[in]  builder  A pointer to the in-process environment builder state
/// @param[in]  path  A C-style string specifying a path to a working directory, to search for modules to load
///
#[no_mangle]
pub extern "C" fn environment_init_add_include_path(builder: *mut env_builder_t, path: *const c_char) {
    let builder_arg_ref = unsafe{ &mut *builder };
    let builder = core::mem::replace(builder_arg_ref, env_builder_t::null()).into_inner();
    let builder = if path.is_null() {
        panic!("Fatal Error: path cannot be NULL");
    } else {
        builder.add_include_paths(vec![PathBuf::from(cstr_as_str(path).borrow())])
    };
    *builder_arg_ref = builder.into();
}

