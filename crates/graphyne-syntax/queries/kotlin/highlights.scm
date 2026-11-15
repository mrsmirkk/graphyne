; Kotlin highlights for tree-sitter-kotlin-ng (the crate ships no query).
; Graphyne resolves overlaps "later pattern wins", so generic captures come first.

(identifier) @variable

[
  (line_comment)
  (block_comment)
  (shebang)
] @comment

[
  (string_literal)
  (multiline_string_literal)
  (character_literal)
] @string

(escape_sequence) @string.escape
(interpolation ["$" "${" "}"] @punctuation.special)

[
  (number_literal)
  (float_literal)
] @number

((identifier) @constant.builtin
  (#any-of? @constant.builtin "true" "false" "null"))

[
  "abstract" "actual" "annotation" "as" "as?" "by" "catch" "class" "companion" "const"
  "constructor" "crossinline" "data" "do" "dynamic" "else" "enum" "expect" "external"
  "final" "finally" "for" "fun" "get" "if" "import" "in" "!in" "infix" "init" "inline"
  "inner" "interface" "internal" "is" "!is" "lateinit" "noinline" "object" "open" "operator" "out"
  "override" "package" "private" "protected" "public" "return" "return@" "sealed" "set"
  "suspend" "tailrec" "throw" "try" "typealias" "val" "value" "var" "vararg" "when"
  "where" "while"
] @keyword

[
  (this_expression)
  (super_expression)
] @variable.builtin

[
  "!" "!!" "!=" "!==" "%" "%=" "&&" "*" "*=" "+" "++" "+=" "-" "--" "-=" "->" ".."
  "..<" "/" "/=" "<" "<=" "=" "==" "===" ">" ">=" "?:" "||" "::" "?."
] @operator

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," ";" ":" "."] @punctuation.delimiter

(package_header (qualified_identifier) @module)
(import (qualified_identifier) @module)

(user_type (identifier) @type)
(class_declaration name: (identifier) @type)
(object_declaration name: (identifier) @type)
(companion_object name: (identifier) @type)
(type_alias type: (identifier) @type)
(enum_entry (identifier) @constant)

(parameter (identifier) @variable.parameter)
(class_parameter (identifier) @variable.parameter)
(value_argument (identifier) @variable.parameter "=")

(function_declaration name: (identifier) @function)
(call_expression (identifier) @function.call)
(call_expression (navigation_expression (identifier) @function.method .))

(annotation "@" @attribute)
(annotation (user_type (identifier) @attribute))
(annotation (constructor_invocation (user_type (identifier) @attribute)))

(label) @label
