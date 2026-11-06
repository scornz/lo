// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/parse/tree_sitter_parser.rs (the language configurations)
//
// MIT License
//
// Copyright (c) 2021-2025 Wilfred Hughes
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! How the syntax-aware diff reads each grammar's trees: difftastic's per-language settings, for
//! the grammars Zed builds in.

/// How the syntax-aware diff reads one tree-sitter grammar's trees.
///
/// [`Language::for_grammar`] has one for each grammar Zed builds in that difftastic supports. Its
/// fields are public, so a caller can describe another grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Language {
    /// The grammar's name in Zed, such as `rust` or `tsx`.
    pub grammar: &'static str,
    /// Node kinds compared as one atom, their children ignored. Grammars give string literals
    /// inner structure, so without this `"x"` and `" x"` would compare equal: only the delimiters
    /// and the `x` would be nodes.
    pub atoms: &'static [&'static str],
    /// The atoms that are strings. A changed string is shown word by word, like a comment.
    pub strings: &'static [&'static str],
    /// Token pairs that open and close a list, such as `(` and `)`.
    pub delimiters: &'static [(&'static str, &'static str)],
    /// Tokens ignored at the end of a node of a kind, as (node kind, token): with `("array",
    /// ",")`, `[1, 2]` and `[1, 2,]` compare equal. Opt-in, because some languages give trailing
    /// tokens meaning, such as Python's `(1,)`.
    pub trailing: &'static [(&'static str, &'static str)],
}

impl Language {
    /// The settings for a grammar Zed builds in, by Zed's name for it, such as `rust`. `None`
    /// for a grammar difftastic doesn't support, such as `markdown`.
    pub fn for_grammar(grammar: &str) -> Option<&'static Language> {
        LANGUAGES
            .iter()
            .find(|language| language.grammar == grammar)
    }
}

const JSON: Language = Language {
    grammar: "json",
    atoms: &["string"],
    strings: &["string"],
    delimiters: &[("{", "}"), ("[", "]")],
    trailing: &[],
};

const TYPESCRIPT: Language = Language {
    grammar: "typescript",
    atoms: &["string", "template_string", "regex", "predefined_type"],
    strings: &["string", "template_string", "regex"],
    delimiters: &[("{", "}"), ("(", ")"), ("[", "]"), ("<", ">")],
    trailing: &[
        ("object", ","),
        ("object_pattern", ","),
        ("array", ","),
        ("array_pattern", ","),
        ("arguments", ","),
        ("formal_parameters", ","),
        ("named_imports", ","),
        ("export_clause", ","),
        ("object_type", ","),
        ("interface_body", ","),
        ("enum_body", ","),
        ("tuple_type", ","),
        ("type_arguments", ","),
    ],
};

/// The grammars Zed builds in that difftastic supports. Zed reads JavaScript with the `tsx`
/// grammar and JSONC with the JSON one.
const LANGUAGES: &[Language] = &[
    Language {
        grammar: "bash",
        atoms: &["string", "raw_string", "heredoc_body", "simple_expansion"],
        strings: &["string", "raw_string", "heredoc_body"],
        delimiters: &[("(", ")"), ("{", "}"), ("[", "]")],
        trailing: &[],
    },
    Language {
        grammar: "c",
        atoms: &["string_literal", "char_literal"],
        strings: &["string_literal", "char_literal"],
        delimiters: &[("(", ")"), ("{", "}"), ("[", "]")],
        trailing: &[("enumerator_list", ","), ("initializer_list", ",")],
    },
    Language {
        grammar: "cpp",
        atoms: &["string_literal", "char_literal", "raw_string_literal"],
        strings: &["string_literal", "char_literal", "raw_string_literal"],
        delimiters: &[("(", ")"), ("{", "}"), ("[", "]"), ("<", ">")],
        trailing: &[("enumerator_list", ","), ("initializer_list", ",")],
    },
    Language {
        grammar: "css",
        atoms: &[
            "integer_value",
            "float_value",
            "color_value",
            "string_value",
        ],
        strings: &["string_value"],
        delimiters: &[("{", "}"), ("(", ")")],
        trailing: &[],
    },
    Language {
        grammar: "go",
        atoms: &["interpreted_string_literal", "raw_string_literal"],
        strings: &["interpreted_string_literal", "raw_string_literal"],
        delimiters: &[("{", "}"), ("[", "]"), ("(", ")")],
        trailing: &[
            ("parameter_list", ","),
            ("argument_list", ","),
            ("literal_value", ","),
        ],
    },
    JSON,
    Language {
        grammar: "jsonc",
        ..JSON
    },
    Language {
        grammar: "python",
        atoms: &["string"],
        strings: &["string"],
        delimiters: &[("(", ")"), ("[", "]"), ("{", "}")],
        // Not tuples, so `(1)` and `(1,)` differ.
        trailing: &[
            ("dictionary", ","),
            ("list", ","),
            ("set", ","),
            ("argument_list", ","),
            ("parameters", ","),
            ("import_from_statement", ","),
        ],
    },
    Language {
        grammar: "rust",
        atoms: &["char_literal", "string_literal", "raw_string_literal"],
        strings: &["char_literal", "string_literal", "raw_string_literal"],
        delimiters: &[("{", "}"), ("(", ")"), ("[", "]"), ("|", "|"), ("<", ">")],
        trailing: &[
            ("arguments", ","),
            ("parameters", ","),
            ("type_parameters", ","),
            ("field_declaration_list", ","),
            ("field_initializer_list", ","),
            ("array_expression", ","),
            ("tuple_expression", ","),
            ("use_list", ","),
            ("enum_variant_list", ","),
            // A macro can't tell `vec![1]` from `vec![1,]`.
            ("token_tree", ","),
        ],
    },
    Language {
        grammar: "tsx",
        ..TYPESCRIPT
    },
    TYPESCRIPT,
    Language {
        grammar: "yaml",
        atoms: &[
            "string_scalar",
            "double_quote_scalar",
            "single_quote_scalar",
            "block_scalar",
        ],
        strings: &[
            "string_scalar",
            "double_quote_scalar",
            "single_quote_scalar",
            "block_scalar",
        ],
        delimiters: &[("{", "}"), ("(", ")"), ("[", "]")],
        trailing: &[("flow_sequence", ","), ("flow_mapping", ",")],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn grammar(name: &str) -> tree_sitter::Language {
        match name {
            "json" | "jsonc" => tree_sitter_json::LANGUAGE.into(),
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            _ => unreachable!(),
        }
    }

    /// Every node kind a setting names exists in the grammar, for the grammars the tests have.
    #[test]
    fn settings_name_node_kinds_that_exist() {
        for name in ["json", "jsonc", "rust", "typescript", "tsx"] {
            let language = Language::for_grammar(name).unwrap();
            let grammar = grammar(name);
            let kinds = language
                .atoms
                .iter()
                .chain(language.strings)
                .chain(language.trailing.iter().map(|(kind, _)| kind));
            for kind in kinds {
                assert_ne!(
                    grammar.id_for_node_kind(kind, true),
                    0,
                    "{name} has no {kind}"
                );
            }
        }
    }

    #[test]
    fn strings_are_atoms() {
        for language in LANGUAGES {
            for string in language.strings {
                assert!(
                    language.atoms.contains(string),
                    "{}: {string}",
                    language.grammar
                );
            }
        }
    }

    #[test]
    fn finds_settings_by_zeds_grammar_name() {
        assert_eq!(Language::for_grammar("rust").unwrap().grammar, "rust");
        assert_eq!(Language::for_grammar("jsonc").unwrap().atoms, ["string"]);
        assert_eq!(Language::for_grammar("markdown"), None);
    }
}
