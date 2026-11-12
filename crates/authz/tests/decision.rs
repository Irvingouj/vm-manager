//! The Authorization decision, tested below the program's outside (the
//! spec's Testing Decisions; blackbox/CONTRACT.md, "Lower seam: the
//! Authorization decision" and "Who is allowed").
//!
//! Three kinds of test:
//!
//! - exhaustive: every question over a small universe (request in flight or
//!   not; every `declared`; registered or not; every `needed`; the User's
//!   Groups and the Grants as lists over a few Groups, with none, one,
//!   several, repeats and look-alike names) is checked against [`rule`], an
//!   independent statement of the contract's table;
//! - one named test per row of the table, and for the ways Groups are
//!   compared;
//! - properties: more Groups or more Grants never turn an Allow into a
//!   Refuse; raising `needed` never turns a Refuse into an Allow; nothing
//!   but the request in flight, `declared`, `registered`, `needed` and the
//!   Grants of the User's own Groups affects the answer. Each property also
//!   checks that it was not satisfied vacuously.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use authz::{AccessLevel, Decision, Grant, Question, Reason, decide};

const LEVELS: [AccessLevel; 3] = [
    AccessLevel::Structure,
    AccessLevel::Read,
    AccessLevel::Write,
];

/// The Groups the exhaustive tests draw from: a name, the same name in
/// another letter case, the same name with a trailing space, and a
/// look-alike with a Cyrillic letter. Compared exactly, these are four
/// different Groups.
const UNIVERSE: [&str; 4] = ["ops", "OPS", "ops ", "оps"];

/// The contract's table, stated on its own terms: the first row that
/// applies refuses; a User holds the highest level any of their Groups is
/// granted, Groups compared byte for byte.
fn rule(q: &Question<'_>) -> Decision {
    if !q.request_in_flight {
        return Decision::Refuse(Reason::NoRequest);
    }
    let Some(declared) = q.declared else {
        return Decision::Refuse(Reason::NotDeclared);
    };
    if !q.registered {
        return Decision::Refuse(Reason::NotRegistered);
    }
    if declared < q.needed {
        return Decision::Refuse(Reason::DeclaredLower {
            declared,
            needed: q.needed,
        });
    }
    let mut held: Option<AccessLevel> = None;
    for grant in q.grants {
        let mine = q
            .groups
            .iter()
            .any(|g| g.as_bytes() == grant.group.as_bytes());
        if mine && held.is_none_or(|h| grant.level > h) {
            held = Some(grant.level);
        }
    }
    match held {
        Some(level) if level >= q.needed => Decision::Allow,
        _ => Decision::Refuse(Reason::NoGrant {
            held,
            needed: q.needed,
        }),
    }
}

/// Every list of length 0 to `max_len` drawn from `items`, repeats allowed.
fn lists<T: Copy>(items: &[T], max_len: usize) -> Vec<Vec<T>> {
    let mut all = vec![Vec::new()];
    let mut last = vec![Vec::new()];
    for _ in 0..max_len {
        let mut next = Vec::new();
        for list in &last {
            for item in items {
                let mut longer: Vec<T> = list.clone();
                longer.push(*item);
                next.push(longer);
            }
        }
        all.extend(next.iter().cloned());
        last = next;
    }
    all
}

fn all_grants() -> Vec<Grant<'static>> {
    UNIVERSE
        .iter()
        .flat_map(|group| {
            LEVELS.iter().map(move |level| Grant {
                group,
                level: *level,
            })
        })
        .collect()
}

fn declared_options() -> [Option<AccessLevel>; 4] {
    [
        None,
        Some(AccessLevel::Structure),
        Some(AccessLevel::Read),
        Some(AccessLevel::Write),
    ]
}

/// Runs `check` on every question of the exhaustive space. Returns how many
/// there were.
fn for_every_question(mut check: impl FnMut(&Question<'_>)) -> usize {
    let group_lists = lists(&UNIVERSE, 3);
    let grant_lists = lists(&all_grants(), 2);
    let mut count = 0;
    for request_in_flight in [false, true] {
        for declared in declared_options() {
            for registered in [false, true] {
                for needed in LEVELS {
                    for groups in &group_lists {
                        for grants in &grant_lists {
                            let q = Question {
                                request_in_flight,
                                groups,
                                declared,
                                registered,
                                grants,
                                needed,
                            };
                            check(&q);
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    count
}

fn describe(q: &Question<'_>) -> String {
    format!("{q:?}")
}

// --- Exhaustive -----------------------------------------------------------

#[test]
fn every_question_is_answered_as_the_contracts_table_says() {
    let mut wrong = Vec::new();
    let mut wrong_count = 0_usize;
    let count = for_every_question(|q| {
        let want = rule(q);
        let got = decide(q);
        if got != want {
            wrong_count += 1;
            if wrong.len() < 10 {
                wrong.push(format!(
                    "  {}\n    expected {want:?}, got {got:?}",
                    describe(q)
                ));
            }
        }
    });
    assert!(
        count > 100_000,
        "the space is smaller than intended: {count}"
    );
    assert!(
        wrong.is_empty(),
        "expected the decision of the contract's table for every one of {count} questions; {wrong_count} differ, the first:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn every_kind_of_answer_occurs_in_the_exhaustive_space() {
    // A check on the test itself: the space covers every row of the table
    // and both ways through the last one.
    let mut seen = std::collections::BTreeSet::new();
    for_every_question(|q| {
        let kind = match rule(q) {
            Decision::Allow => "allow",
            Decision::Refuse(Reason::NoRequest) => "no-request",
            Decision::Refuse(Reason::NotDeclared) => "not-declared",
            Decision::Refuse(Reason::NotRegistered) => "not-registered",
            Decision::Refuse(Reason::DeclaredLower { .. }) => "declared-lower",
            Decision::Refuse(Reason::NoGrant { held: None, .. }) => "no-grant, holding nothing",
            Decision::Refuse(Reason::NoGrant { held: Some(_), .. }) => "no-grant, holding less",
        };
        seen.insert(kind);
    });
    assert_eq!(seen.len(), 7, "the space covers only {seen:?}");
}

// --- One test per row of the table ----------------------------------------

/// A question that is allowed: a request in flight, `read` declared and
/// registered, the User in `ops`, which is granted `read`, and `read`
/// needed. Each named test changes one thing.
fn allowed<'a>(groups: &'a [&'a str], grants: &'a [Grant<'a>]) -> Question<'a> {
    Question {
        request_in_flight: true,
        groups,
        declared: Some(AccessLevel::Read),
        registered: true,
        grants,
        needed: AccessLevel::Read,
    }
}

const OPS_READ: [Grant<'static>; 1] = [Grant {
    group: "ops",
    level: AccessLevel::Read,
}];

#[test]
fn a_user_in_a_granted_group_with_the_resource_declared_and_registered_is_allowed() {
    let q = allowed(&["ops"], &OPS_READ);
    assert_eq!(decide(&q), Decision::Allow, "{}", describe(&q));
}

#[test]
fn row_no_request_with_no_request_in_flight_whatever_else_holds() {
    let q = Question {
        request_in_flight: false,
        ..allowed(&["ops"], &OPS_READ)
    };
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoRequest),
        "{}",
        describe(&q)
    );
    // It comes first: also with nothing declared, nothing registered, no Grant.
    let q = Question {
        request_in_flight: false,
        groups: &[],
        declared: None,
        registered: false,
        grants: &[],
        needed: AccessLevel::Write,
    };
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoRequest),
        "{}",
        describe(&q)
    );
}

#[test]
fn row_not_declared_whether_or_not_the_resource_is_registered() {
    for registered in [true, false] {
        let q = Question {
            declared: None,
            registered,
            ..allowed(&["ops"], &OPS_READ)
        };
        assert_eq!(
            decide(&q),
            Decision::Refuse(Reason::NotDeclared),
            "{}",
            describe(&q)
        );
    }
}

#[test]
fn row_not_registered_when_declared_but_no_such_resource() {
    for declared in [
        AccessLevel::Structure,
        AccessLevel::Read,
        AccessLevel::Write,
    ] {
        let q = Question {
            declared: Some(declared),
            registered: false,
            ..allowed(&["ops"], &OPS_READ)
        };
        // Before declared-lower: `structure` declared and not registered is
        // not-registered.
        assert_eq!(
            decide(&q),
            Decision::Refuse(Reason::NotRegistered),
            "{}",
            describe(&q)
        );
    }
}

#[test]
fn row_declared_lower_carries_the_declared_and_the_needed_level() {
    let q = Question {
        declared: Some(AccessLevel::Structure),
        ..allowed(&["ops"], &OPS_READ)
    };
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::DeclaredLower {
            declared: AccessLevel::Structure,
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
    let q = Question {
        declared: Some(AccessLevel::Read),
        needed: AccessLevel::Write,
        ..allowed(
            &["ops"],
            &[Grant {
                group: "ops",
                level: AccessLevel::Write,
            }],
        )
    };
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::DeclaredLower {
            declared: AccessLevel::Read,
            needed: AccessLevel::Write
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn declared_lower_comes_before_no_grant() {
    let q = Question {
        declared: Some(AccessLevel::Structure),
        ..allowed(&[], &[])
    };
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::DeclaredLower {
            declared: AccessLevel::Structure,
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn a_declared_level_above_the_needed_one_is_enough() {
    let q = Question {
        declared: Some(AccessLevel::Write),
        ..allowed(&["ops"], &OPS_READ)
    };
    assert_eq!(decide(&q), Decision::Allow, "{}", describe(&q));
}

#[test]
fn row_no_grant_for_a_user_in_no_group_holds_nothing() {
    let q = allowed(&[], &OPS_READ);
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoGrant {
            held: None,
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn row_no_grant_for_a_user_whose_groups_hold_no_grant() {
    let q = allowed(&["finance", "everyone"], &OPS_READ);
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoGrant {
            held: None,
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn row_no_grant_for_a_resource_with_no_grants_at_all() {
    let q = allowed(&["ops"], &[]);
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoGrant {
            held: None,
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn row_no_grant_for_a_user_whose_only_grant_is_structure_says_it_is_held() {
    let q = allowed(
        &["ops"],
        &[Grant {
            group: "ops",
            level: AccessLevel::Structure,
        }],
    );
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoGrant {
            held: Some(AccessLevel::Structure),
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn no_grant_reports_the_highest_level_held_below_the_one_needed() {
    let grants = [
        Grant {
            group: "a",
            level: AccessLevel::Structure,
        },
        Grant {
            group: "b",
            level: AccessLevel::Read,
        },
    ];
    let q = Question {
        declared: Some(AccessLevel::Write),
        needed: AccessLevel::Write,
        ..allowed(&["a", "b"], &grants)
    };
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoGrant {
            held: Some(AccessLevel::Read),
            needed: AccessLevel::Write
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn a_user_in_several_groups_holds_the_highest_level_any_of_them_is_granted() {
    let grants = [
        Grant {
            group: "a",
            level: AccessLevel::Structure,
        },
        Grant {
            group: "b",
            level: AccessLevel::Write,
        },
        Grant {
            group: "c",
            level: AccessLevel::Read,
        },
    ];
    for groups in [&["a", "b"][..], &["b", "a"], &["c", "a"], &["x", "a", "c"]] {
        let q = allowed(groups, &grants);
        assert_eq!(decide(&q), Decision::Allow, "{}", describe(&q));
    }
    // `write` includes `read` and `structure`.
    for needed in LEVELS {
        let q = Question {
            declared: Some(AccessLevel::Write),
            needed,
            ..allowed(&["b"], &grants)
        };
        assert_eq!(decide(&q), Decision::Allow, "{}", describe(&q));
    }
}

#[test]
fn the_order_of_grants_does_not_matter_for_the_highest_level() {
    let low_first = [
        Grant {
            group: "ops",
            level: AccessLevel::Structure,
        },
        Grant {
            group: "ops",
            level: AccessLevel::Read,
        },
    ];
    let high_first = [low_first[1], low_first[0]];
    for grants in [&low_first[..], &high_first[..]] {
        let q = allowed(&["ops"], grants);
        assert_eq!(decide(&q), Decision::Allow, "{}", describe(&q));
    }
}

#[test]
fn groups_are_compared_exactly() {
    // Letter case, surrounding whitespace, look-alike letters, a prefix and
    // an extension are all other Groups.
    for group in [
        "OPS",
        "Ops",
        " ops",
        "ops ",
        "оps",
        "ops\u{200b}",
        "op",
        "opss",
        "ops,x",
        "",
    ] {
        let groups = [group];
        let q = allowed(&groups, &OPS_READ);
        assert_eq!(
            decide(&q),
            Decision::Refuse(Reason::NoGrant {
                held: None,
                needed: AccessLevel::Read
            }),
            "a User in {group:?} holds no Grant given to \"ops\": {}",
            describe(&q)
        );
    }
}

#[test]
fn a_grant_to_a_look_alike_group_does_not_reach_the_real_one() {
    let grants = [Grant {
        group: "оps",
        level: AccessLevel::Write,
    }];
    let q = allowed(&["ops"], &grants);
    assert_eq!(
        decide(&q),
        Decision::Refuse(Reason::NoGrant {
            held: None,
            needed: AccessLevel::Read
        }),
        "{}",
        describe(&q)
    );
}

#[test]
fn repeated_groups_and_repeated_grants_change_nothing() {
    let grants = [OPS_READ[0], OPS_READ[0]];
    let q = allowed(&["ops", "ops", "ops"], &grants);
    assert_eq!(decide(&q), Decision::Allow, "{}", describe(&q));
}

// --- Properties -------------------------------------------------------------

#[test]
fn more_groups_never_turn_an_allow_into_a_refuse() {
    let mut allows_checked = 0_usize;
    let mut broken = Vec::new();
    for_every_question(|q| {
        if decide(q) != Decision::Allow {
            return;
        }
        for extra in UNIVERSE.iter().chain(["unrelated"].iter()) {
            let mut more: Vec<&str> = q.groups.to_vec();
            more.push(extra);
            let bigger = Question {
                groups: &more,
                ..*q
            };
            allows_checked += 1;
            if decide(&bigger) != Decision::Allow && broken.len() < 5 {
                broken.push(format!("{} -> {:?}", describe(&bigger), decide(&bigger)));
            }
        }
    });
    assert!(
        allows_checked > 0,
        "expected some questions to be allowed, so that adding a Group could be checked against an Allow; none was"
    );
    assert!(
        broken.is_empty(),
        "adding a Group turned an Allow into:\n{}",
        broken.join("\n")
    );
}

#[test]
fn more_grants_never_turn_an_allow_into_a_refuse() {
    let mut allows_checked = 0_usize;
    let mut broken = Vec::new();
    let extras = all_grants();
    for_every_question(|q| {
        if decide(q) != Decision::Allow {
            return;
        }
        for extra in &extras {
            let mut more: Vec<Grant<'_>> = q.grants.to_vec();
            more.push(*extra);
            let bigger = Question {
                grants: &more,
                ..*q
            };
            allows_checked += 1;
            if decide(&bigger) != Decision::Allow && broken.len() < 5 {
                broken.push(format!("{} -> {:?}", describe(&bigger), decide(&bigger)));
            }
        }
    });
    assert!(
        allows_checked > 0,
        "expected some questions to be allowed, so that adding a Grant could be checked against an Allow; none was"
    );
    assert!(
        broken.is_empty(),
        "adding a Grant turned an Allow into:\n{}",
        broken.join("\n")
    );
}

#[test]
fn raising_the_level_needed_never_turns_a_refuse_into_an_allow() {
    let mut refuses_raised = 0_usize;
    let mut allows_seen = 0_usize;
    let mut broken = Vec::new();
    for_every_question(|q| {
        let here = decide(q);
        if here == Decision::Allow {
            allows_seen += 1;
            return;
        }
        for higher in LEVELS.iter().filter(|l| **l > q.needed) {
            let raised = Question {
                needed: *higher,
                ..*q
            };
            refuses_raised += 1;
            if decide(&raised) == Decision::Allow && broken.len() < 5 {
                broken.push(format!(
                    "{} was {here:?}, needing {higher:?} is Allow",
                    describe(q)
                ));
            }
        }
    });
    assert!(
        allows_seen > 0 && refuses_raised > 0,
        "expected both allowed and refused questions in the space (allowed: {allows_seen}, refusals raised: {refuses_raised}), so that the property is not satisfied vacuously"
    );
    assert!(
        broken.is_empty(),
        "raising `needed` turned a Refuse into an Allow:\n{}",
        broken.join("\n")
    );
}

#[test]
fn nothing_but_the_inputs_the_contract_names_affects_the_answer() {
    // The answer depends on `request_in_flight`, `declared`, `registered`,
    // `needed` and the Grants of the User's own Groups, and on nothing else:
    // not the Grants of other Groups, not Groups that hold no Grant, not the
    // order or repetition of either list.
    let mut kinds = std::collections::BTreeSet::new();
    let mut broken = Vec::new();
    for_every_question(|q| {
        let here = decide(q);
        kinds.insert(format!("{here:?}"));
        let own_grants: Vec<Grant<'_>> = q
            .grants
            .iter()
            .copied()
            .filter(|g| q.groups.contains(&g.group))
            .collect();
        let granted_groups: Vec<&str> = q
            .groups
            .iter()
            .copied()
            .filter(|g| q.grants.iter().any(|grant| grant.group == *g))
            .collect();
        let mut reversed_groups: Vec<&str> = q.groups.to_vec();
        reversed_groups.reverse();
        let mut reversed_grants: Vec<Grant<'_>> = q.grants.to_vec();
        reversed_grants.reverse();
        let mut doubled_groups: Vec<&str> = q.groups.to_vec();
        doubled_groups.extend_from_slice(q.groups);
        let variants = [
            (
                "only the Grants of the User's own Groups",
                Question {
                    grants: &own_grants,
                    ..*q
                },
            ),
            (
                "only the User's Groups that hold a Grant",
                Question {
                    groups: &granted_groups,
                    ..*q
                },
            ),
            (
                "the Groups in reverse order",
                Question {
                    groups: &reversed_groups,
                    ..*q
                },
            ),
            (
                "the Grants in reverse order",
                Question {
                    grants: &reversed_grants,
                    ..*q
                },
            ),
            (
                "every Group stated twice",
                Question {
                    groups: &doubled_groups,
                    ..*q
                },
            ),
        ];
        for (what, variant) in variants {
            let there = decide(&variant);
            if there != here && broken.len() < 5 {
                broken.push(format!(
                    "{}: {here:?}\n  with {what}: {there:?}",
                    describe(q)
                ));
            }
        }
    });
    assert!(
        kinds.len() >= 6,
        "expected the decision to give the contract's different answers across the space, so that this property is not satisfied by a constant; it gave only {kinds:?}"
    );
    assert!(
        broken.is_empty(),
        "expected the same answer when only things the decision must ignore change, got:\n{}",
        broken.join("\n")
    );
}
