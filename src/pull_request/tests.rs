use super::*;

/// A detail as T3's GitHub provider sends it, with every optional field filled in.
fn detail_value() -> Value {
    json!({
        "provider": "github",
        "capabilities": {
            "diff": true,
            "comment": true,
            "actions": ["merge", "ready", "draft", "close", "reopen", "update-branch",
                "enable-auto-merge", "disable-auto-merge", "revert", "approve-workflows"],
            "mergeMethods": ["merge", "squash", "rebase"],
            "updateMethods": ["merge", "rebase"],
            "search": true,
            "reactions": true,
            "viewedFiles": "host",
            "review": {"inlineComment": true, "reply": true, "resolve": true,
                "verdicts": ["comment", "approve", "request-changes"]},
            "reviewers": {"request": true, "listCandidates": true},
            "edit": {"changeRequest": true, "comment": true},
            "stacks": true,
            "stackActions": false,
            "labels": true
        },
        "viewerPermissions": {
            "stackRebase": false,
            "actions": ["merge", "close"],
            "comment": true,
            "resolve": true,
            "verdicts": ["comment", "approve"],
            "requestReviewers": true,
            "updateMethods": ["rebase"],
            "labels": false
        },
        "projectId": "project-1",
        "projectTitle": "t3term",
        "workspaceRoot": "/work/t3term",
        "repository": "krishhgg/t3term",
        "number": 41,
        "title": "Add a terminal drawer",
        "body": "What it changes.\n\n- one\n- two",
        "url": "https://github.com/krishhgg/t3term/pull/41",
        "author": {"login": "octo", "name": "Octo Cat", "avatarUrl": "https://example.com/a.png"},
        "state": "open",
        "isDraft": true,
        "mergeability": "mergeable",
        "additions": 120,
        "deletions": 7,
        "changedFiles": 4,
        "headBranch": "t3term/embedded-terminal-pane",
        "headRepositoryNameWithOwner": "octo/t3term",
        "headSha": "9dc565a0",
        "baseBranch": "master",
        "createdAt": "2026-10-13T08:00:00Z",
        "updatedAt": "2026-10-14T23:57:28Z",
        "observedAt": 1_792_000_000_000_u64,
        "mergedAt": null,
        "closedAt": null,
        "reviewers": [
            {"login": "greptile-apps", "name": null, "avatarUrl": null, "isBot": true},
            {"login": "reviewer", "name": "Re Viewer", "avatarUrl": null}
        ],
        "labels": [{"name": "terminal", "color": "0e8a16"}, {"name": "ui", "color": null}],
        "checks": [
            {"name": "macOS", "status": "success", "description": "Passed in 6m",
                "url": "https://github.com/krishhgg/t3term/actions/runs/1", "required": true},
            {"name": "Linux build (unsupported)", "status": "pending", "description": null,
                "url": null}
        ],
        "mergeCapabilities": {"merge": true, "squash": true, "rebase": false},
        "viewer": "octo",
        "baseComparison": "behind",
        "behindBy": 3,
        "autoMergeEnabled": false,
        "autoMergeMethod": "squash",
        "workflowApprovalsRequired": 0
    })
}

/// An activity as T3 sends it, with a comment, a review, a thread and a commit.
fn activity_value() -> Value {
    json!({
        "author": {"login": "octo", "name": "Octo Cat", "avatarUrl": "https://example.com/a.png"},
        "reviewers": [{"login": "reviewer", "name": null, "avatarUrl": null}],
        "comments": [
            {"id": "c1", "kind": "issue-comment", "author": {"login": "octo", "name": null,
                "avatarUrl": null}, "body": "Ready for a look.", "createdAt": "2026-10-14T01:00:00Z",
                "editedAt": null, "url": "https://github.com/krishhgg/t3term/pull/41#c1",
                "path": null, "reviewState": null,
                "reactions": [{"content": "rocket", "count": 2, "actors": ["reviewer"],
                    "viewerHasReacted": true}]},
            {"id": "r1", "kind": "review", "author": null, "body": "",
                "createdAt": "2026-10-14T02:00:00Z", "url": null, "path": null,
                "reviewState": "APPROVED"}
        ],
        "commentCount": 5,
        "commentsTruncated": true,
        "reviewThreadsTruncated": false,
        "reviewThreads": [
            {"id": "t1", "path": "src/tui/terminal.rs", "line": 673, "side": "right",
                "isResolved": false, "isOutdated": true,
                "comments": [{"id": "tc1", "author": {"login": "greptile-apps", "name": null,
                    "avatarUrl": null, "isBot": true}, "body": "Start target misses button",
                    "createdAt": "2026-10-14T23:57:28Z", "editedAt": "2026-10-15T00:00:00Z",
                    "url": null}],
                "commentCount": 3,
                "nextCommentsCursor": "cursor-2"},
            {"id": "t2", "path": "README.md", "line": null, "side": "left",
                "isResolved": true, "isOutdated": false, "comments": []}
        ],
        "commits": [
            {"oid": "9dc565a0", "messageHeadline": "Wrap the missing-terminal rows",
                "committedDate": "2026-10-14T23:00:00Z", "additions": 40, "deletions": 3,
                "authors": [{"login": "octo", "name": null, "avatarUrl": null}]},
            {"oid": "05a9899", "messageHeadline": "", "committedDate": "2026-10-14T20:00:00Z"}
        ],
        "reactions": [{"content": "heart", "count": 1, "actors": [], "viewerHasReacted": false}]
    })
}

fn failed(error: Value) -> RpcError {
    let message = error["detail"]
        .as_str()
        .unwrap_or("T3 rejected pullRequests.detail.")
        .to_string();
    RpcError::Failed {
        tag: DETAIL.to_string(),
        message,
        error_tag: error["_tag"].as_str().map(str::to_string),
        error: Box::new(error),
    }
}

fn malformed_field(value: &Value) -> String {
    match Detail::read(value) {
        Err(PullRequestError::Malformed(what)) => what,
        other => panic!("expected a malformed answer, got {other:?}"),
    }
}

#[test]
fn a_reference_sends_the_fields_t3s_pull_request_ref_takes() {
    let reference = Reference::new(" project-1 ", None, " owner/repo ", 7).unwrap();
    assert_eq!(
        reference.payload(),
        json!({"projectId": "project-1", "repository": "owner/repo", "number": 7})
    );
    let routed = Reference::new("project-1", Some("github.com"), "other/repo", 9).unwrap();
    assert_eq!(
        routed.payload(),
        json!({"projectId": "project-1", "host": "github.com", "repository": "other/repo",
            "number": 9})
    );
    assert_eq!(
        (
            routed.project_id(),
            routed.host(),
            routed.repository(),
            routed.number()
        ),
        ("project-1", Some("github.com"), "other/repo", 9)
    );
    let largest = Reference::new("p", None, "o/r", MAX_SAFE_INTEGER).unwrap();
    assert_eq!(
        largest.payload()["number"],
        json!(9_007_199_254_740_991_u64)
    );
}

#[test]
fn a_reference_that_breaks_t3s_schema_sends_nothing() {
    let long = "x".repeat(KEY + 1);
    let cases = [
        Reference::new("  ", None, "o/r", 1),
        Reference::new("p", Some(" "), "o/r", 1),
        Reference::new("p", None, "", 1),
        Reference::new("p", None, "o/r", 0),
        Reference::new("p", None, "o/r", MAX_SAFE_INTEGER + 1),
        Reference::new(&long, None, "o/r", 1),
        Reference::new("p", Some(&long), "o/r", 1),
        Reference::new("p", None, &long, 1),
    ];
    for case in cases {
        assert!(
            matches!(case, Err(PullRequestError::Invalid(_))),
            "{case:?}"
        );
    }
    assert!(Reference::new(&"x".repeat(KEY), None, "o/r", 1).is_ok());
}

#[test]
fn a_detail_reads_every_field_t3_sends() {
    let detail = Detail::read(&detail_value()).unwrap();
    assert_eq!(detail.provider, Provider::Github);
    assert_eq!(
        detail.capabilities,
        Capabilities {
            diff: true,
            comment: true,
            actions: vec![
                Action::Merge,
                Action::Ready,
                Action::Draft,
                Action::Close,
                Action::Reopen,
                Action::UpdateBranch,
                Action::EnableAutoMerge,
                Action::DisableAutoMerge,
                Action::Revert,
                Action::ApproveWorkflows,
            ],
            merge_methods: vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
            update_methods: vec![UpdateMethod::Merge, UpdateMethod::Rebase],
            search: true,
            reactions: true,
            viewed_files: Some(ViewedFilesStore::Host),
            review: ReviewCapabilities {
                inline_comment: true,
                reply: true,
                resolve: true,
                verdicts: vec![Verdict::Comment, Verdict::Approve, Verdict::RequestChanges],
            },
            reviewers: ReviewerCapabilities {
                request: true,
                list_candidates: true,
            },
            edit_change_request: true,
            edit_comment: true,
            labels: true,
        }
    );
    assert_eq!(
        detail.permissions,
        Permissions {
            actions: vec![Action::Merge, Action::Close],
            comment: true,
            resolve: true,
            verdicts: vec![Verdict::Comment, Verdict::Approve],
            request_reviewers: true,
            update_methods: vec![UpdateMethod::Rebase],
            labels: false,
        }
    );
    assert_eq!(
        (
            detail.project_id.as_str(),
            detail.project_title.as_str(),
            detail.workspace_root.as_str(),
            detail.repository.as_str(),
            detail.number,
        ),
        ("project-1", "t3term", "/work/t3term", "krishhgg/t3term", 41)
    );
    assert_eq!(detail.title, "Add a terminal drawer");
    assert_eq!(
        detail.body,
        Body {
            text: "What it changes.\n\n- one\n- two".to_string(),
            cut: false
        }
    );
    assert_eq!(detail.url, "https://github.com/krishhgg/t3term/pull/41");
    assert_eq!(
        detail.author,
        Some(Actor {
            login: "octo".to_string(),
            name: Some("Octo Cat".to_string()),
            is_bot: false,
        })
    );
    assert_eq!(
        (detail.state, detail.is_draft, detail.mergeability),
        (State::Open, true, Mergeability::Mergeable)
    );
    assert_eq!(
        (detail.additions, detail.deletions, detail.changed_files),
        (120, 7, 4)
    );
    assert_eq!(detail.head_branch, "t3term/embedded-terminal-pane");
    assert_eq!(detail.head_repository.as_deref(), Some("octo/t3term"));
    assert_eq!(detail.head_sha.as_deref(), Some("9dc565a0"));
    assert_eq!(detail.base_branch, "master");
    assert_eq!(detail.created_at, "2026-10-13T08:00:00Z");
    assert_eq!(detail.updated_at, "2026-10-14T23:57:28Z");
    assert_eq!((detail.merged_at, detail.closed_at), (None, None));
    assert_eq!(
        detail.reviewers,
        vec![
            Actor {
                login: "greptile-apps".to_string(),
                name: None,
                is_bot: true,
            },
            Actor {
                login: "reviewer".to_string(),
                name: Some("Re Viewer".to_string()),
                is_bot: false,
            },
        ]
    );
    assert_eq!(
        detail.labels,
        vec![
            Label {
                name: "terminal".to_string(),
                color: Some("0e8a16".to_string()),
            },
            Label {
                name: "ui".to_string(),
                color: None,
            },
        ]
    );
    assert_eq!(
        detail.checks,
        vec![
            Check {
                name: "macOS".to_string(),
                status: CheckStatus::Success,
                description: Some("Passed in 6m".to_string()),
                url: Some("https://github.com/krishhgg/t3term/actions/runs/1".to_string()),
                required: Some(true),
            },
            Check {
                name: "Linux build (unsupported)".to_string(),
                status: CheckStatus::Pending,
                description: None,
                url: None,
                required: None,
            },
        ]
    );
    assert_eq!(
        detail.merge_capabilities,
        MergeCapabilities {
            merge: true,
            squash: true,
            rebase: false,
        }
    );
    assert_eq!(detail.viewer.as_deref(), Some("octo"));
    assert_eq!(detail.base_comparison, Some(BaseComparison::Behind));
    assert_eq!(detail.behind_by, Some(3));
    assert_eq!(detail.auto_merge, Some(false));
    assert_eq!(detail.auto_merge_method, Some(MergeMethod::Squash));
    assert_eq!(detail.workflow_approvals, Some(0));
    assert!(!detail.cut);
}

#[test]
fn optional_fields_t3_leaves_out_read_as_the_schema_says() {
    let mut value = detail_value();
    let object = value.as_object_mut().unwrap();
    for key in [
        "headRepositoryNameWithOwner",
        "headSha",
        "viewer",
        "baseComparison",
        "behindBy",
        "autoMergeEnabled",
        "autoMergeMethod",
        "workflowApprovalsRequired",
    ] {
        object.remove(key);
    }
    object["author"] = Value::Null;
    object["mergedAt"] = json!("2026-10-15T00:00:00Z");
    let capabilities = object["capabilities"].as_object_mut().unwrap();
    for key in [
        "updateMethods",
        "reactions",
        "viewedFiles",
        "edit",
        "labels",
    ] {
        capabilities.remove(key);
    }
    let permissions = object["viewerPermissions"].as_object_mut().unwrap();
    permissions.remove("updateMethods");
    permissions.remove("labels");

    let detail = Detail::read(&value).unwrap();
    assert_eq!(detail.author, None);
    assert_eq!(detail.merged_at.as_deref(), Some("2026-10-15T00:00:00Z"));
    assert_eq!(
        (
            detail.head_repository,
            detail.head_sha,
            detail.viewer,
            detail.base_comparison,
            detail.behind_by,
            detail.auto_merge,
            detail.auto_merge_method,
            detail.workflow_approvals,
        ),
        (None, None, None, None, None, None, None, None)
    );
    // A server that says nothing about a feature has none.
    let capabilities = &detail.capabilities;
    assert!(capabilities.update_methods.is_empty());
    assert!(!capabilities.reactions);
    assert_eq!(capabilities.viewed_files, None);
    assert!(!capabilities.edit_change_request && !capabilities.edit_comment);
    assert!(!capabilities.labels);
    // A permission the host reports nothing about is granted, except updating the branch,
    // which absent says the viewer may not.
    assert!(detail.permissions.update_methods.is_empty());
    assert!(detail.permissions.labels);
}

#[test]
fn a_detail_without_a_field_t3_requires_is_malformed() {
    let required = [
        "provider",
        "capabilities",
        "viewerPermissions",
        "projectId",
        "projectTitle",
        "workspaceRoot",
        "repository",
        "number",
        "title",
        "body",
        "url",
        "author",
        "state",
        "isDraft",
        "mergeability",
        "additions",
        "deletions",
        "changedFiles",
        "headBranch",
        "baseBranch",
        "createdAt",
        "updatedAt",
        "mergedAt",
        "closedAt",
        "reviewers",
        "labels",
        "checks",
        "mergeCapabilities",
    ];
    for key in required {
        let mut value = detail_value();
        value.as_object_mut().unwrap().remove(key);
        assert!(
            malformed_field(&value).ends_with(key),
            "{key}: {}",
            malformed_field(&value)
        );
    }
    let wrong = [
        ("number", json!(0)),
        ("number", json!(-1)),
        ("number", json!(1.5)),
        ("number", json!(9_007_199_254_740_992_u64)),
        ("title", json!("   ")),
        ("title", json!(7)),
        ("additions", json!("120")),
        ("isDraft", json!("true")),
        ("state", json!("draft")),
        ("mergeability", json!(null)),
        ("checks", json!({})),
        ("baseComparison", json!(1)),
        ("headSha", json!(" ")),
    ];
    for (key, bad) in wrong {
        let mut value = detail_value();
        value[key] = bad.clone();
        assert!(Detail::read(&value).is_err(), "{key}: {bad}");
    }
    // A field T3 requires may be null, but not left out.
    let mut value = detail_value();
    value["checks"][1].as_object_mut().unwrap().remove("url");
    assert_eq!(malformed_field(&value), "a check has no valid url");
    let mut value = detail_value();
    value["reviewers"][0]
        .as_object_mut()
        .unwrap()
        .remove("name");
    assert_eq!(malformed_field(&value), "a person has no valid name");
    let mut value = detail_value();
    value["checks"][1]["status"] = json!("stale");
    assert_eq!(malformed_field(&value), "a check has no valid status");
    let mut value = detail_value();
    value["capabilities"]["review"]["verdicts"] = json!("approve");
    assert_eq!(malformed_field(&value), "a review has no valid verdicts");
    assert_eq!(malformed_field(&json!([])), "a detail isn't an object");
}

#[test]
fn values_this_version_does_not_know_are_left_out() {
    let mut value = detail_value();
    value["capabilities"]["actions"] = json!(["merge", "stack-rebase", "merge", "close"]);
    value["capabilities"]["mergeMethods"] = json!(["fast-forward"]);
    value["viewerPermissions"]["verdicts"] = json!(["approve", "dismiss"]);
    value["capabilities"]["viewedFiles"] = json!("browser");
    value["baseComparison"] = json!("diverged");
    value["autoMergeMethod"] = json!("fast-forward");
    value["provider"] = json!("gitlab");
    let detail = Detail::read(&value).unwrap();
    assert_eq!(
        detail.capabilities.actions,
        vec![Action::Merge, Action::Close]
    );
    assert!(detail.capabilities.merge_methods.is_empty());
    assert_eq!(detail.permissions.verdicts, vec![Verdict::Approve]);
    assert_eq!(detail.capabilities.viewed_files, None);
    assert_eq!(detail.base_comparison, None);
    assert_eq!(detail.auto_merge_method, None);
    assert_eq!(detail.provider, Provider::Gitlab);

    let mut value = activity_value();
    value["comments"][0]["reactions"] = json!([
        {"content": "party-parrot", "count": 3, "actors": [], "viewerHasReacted": false},
        {"content": "eyes", "count": 1, "actors": ["octo"], "viewerHasReacted": false}
    ]);
    let activity = Activity::read(&value).unwrap();
    assert_eq!(
        activity.comments[0].reactions,
        Some(vec![Reaction {
            content: ReactionContent::Eyes,
            count: 1,
            actors: vec!["octo".to_string()],
            viewer_reacted: false,
        }])
    );
    // A reaction without its content is still malformed, known or not.
    value["comments"][0]["reactions"][0]
        .as_object_mut()
        .unwrap()
        .remove("content");
    assert!(Activity::read(&value).is_err());
}

#[test]
fn text_is_cut_on_a_character_boundary_and_says_so() {
    let mut value = detail_value();
    // 262,143 bytes of `a`, then a two-byte `é` that would end one byte past the bound.
    value["body"] = json!(format!("{}é and more", "a".repeat(BODY - 1)));
    value["title"] = json!("t".repeat(LINE + 10));
    value["headBranch"] = json!(format!("{}ü", "b".repeat(NAME - 1)));
    let detail = Detail::read(&value).unwrap();
    assert_eq!(detail.body.text.len(), BODY - 1);
    assert!(detail.body.cut);
    assert_eq!(detail.title.len(), LINE);
    assert_eq!(detail.head_branch, "b".repeat(NAME - 1));

    let exact = "é".repeat(BODY / 2);
    value["body"] = json!(exact);
    let detail = Detail::read(&value).unwrap();
    assert_eq!(detail.body.text, exact);
    assert!(!detail.body.cut);
}

#[test]
fn lists_past_their_bound_are_cut_and_the_rest_left_unread() {
    let mut value = detail_value();
    let check = json!({"name": "c", "status": "success", "description": null, "url": null});
    let mut checks = vec![check; MOST_CHECKS];
    // Past the bound, so never read.
    checks.push(json!({"name": "", "status": "nonsense"}));
    value["checks"] = json!(checks);
    let detail = Detail::read(&value).unwrap();
    assert_eq!(detail.checks.len(), MOST_CHECKS);
    assert!(detail.cut);

    value["checks"] = json!([]);
    let reviewer = json!({"login": "r", "name": null, "avatarUrl": null});
    value["reviewers"] = json!(vec![reviewer; MOST_PEOPLE + 1]);
    let detail = Detail::read(&value).unwrap();
    assert_eq!(detail.reviewers.len(), MOST_PEOPLE);
    assert!(detail.cut);

    let mut value = activity_value();
    let comment = value["comments"][1].clone();
    value["comments"] = json!(vec![comment; MOST_COMMENTS + 1]);
    let thread_comment = value["reviewThreads"][0]["comments"][0].clone();
    value["reviewThreads"][0]["comments"] = json!(vec![thread_comment; MOST_THREAD_COMMENTS + 1]);
    let activity = Activity::read(&value).unwrap();
    assert_eq!(activity.comments.len(), MOST_COMMENTS);
    assert!(activity.cut);
    assert_eq!(activity.threads[0].comments.len(), MOST_THREAD_COMMENTS);
    assert!(activity.threads[0].cut);
    assert!(!activity.threads[1].cut);
}

#[test]
fn an_activity_reads_its_remarks_threads_and_commits() {
    let activity = Activity::read(&activity_value()).unwrap();
    let octo = Actor {
        login: "octo".to_string(),
        name: None,
        is_bot: false,
    };
    assert_eq!(
        activity.author,
        Some(Actor {
            name: Some("Octo Cat".to_string()),
            ..octo.clone()
        })
    );
    assert_eq!(
        activity.reviewers,
        Some(vec![Actor {
            login: "reviewer".to_string(),
            name: None,
            is_bot: false,
        }])
    );
    assert_eq!(
        activity.comments,
        vec![
            Comment {
                id: "c1".to_string(),
                kind: CommentKind::IssueComment,
                author: Some(octo.clone()),
                body: Body {
                    text: "Ready for a look.".to_string(),
                    cut: false,
                },
                created_at: "2026-10-14T01:00:00Z".to_string(),
                edited_at: None,
                url: Some("https://github.com/krishhgg/t3term/pull/41#c1".to_string()),
                path: None,
                review_state: None,
                reactions: Some(vec![Reaction {
                    content: ReactionContent::Rocket,
                    count: 2,
                    actors: vec!["reviewer".to_string()],
                    viewer_reacted: true,
                }]),
            },
            Comment {
                id: "r1".to_string(),
                kind: CommentKind::Review,
                author: None,
                body: Body::default(),
                created_at: "2026-10-14T02:00:00Z".to_string(),
                edited_at: None,
                url: None,
                path: None,
                review_state: Some("APPROVED".to_string()),
                reactions: None,
            },
        ]
    );
    assert_eq!(
        (activity.comment_count, activity.comments_truncated),
        (5, true)
    );
    assert!(!activity.threads_truncated);
    assert_eq!(
        activity.threads,
        vec![
            ReviewThread {
                id: "t1".to_string(),
                path: "src/tui/terminal.rs".to_string(),
                line: Some(673),
                side: Side::Right,
                resolved: false,
                outdated: true,
                comments: vec![ThreadComment {
                    id: "tc1".to_string(),
                    author: Some(Actor {
                        login: "greptile-apps".to_string(),
                        name: None,
                        is_bot: true,
                    }),
                    body: Body {
                        text: "Start target misses button".to_string(),
                        cut: false,
                    },
                    created_at: "2026-10-14T23:57:28Z".to_string(),
                    edited_at: Some("2026-10-15T00:00:00Z".to_string()),
                    url: None,
                    reactions: None,
                }],
                comment_count: Some(3),
                next_comments: Some("cursor-2".to_string()),
                cut: false,
            },
            ReviewThread {
                id: "t2".to_string(),
                path: "README.md".to_string(),
                line: None,
                side: Side::Left,
                resolved: true,
                outdated: false,
                comments: vec![],
                comment_count: None,
                next_comments: None,
                cut: false,
            },
        ]
    );
    assert_eq!(
        activity.commits,
        vec![
            Commit {
                oid: "9dc565a0".to_string(),
                headline: "Wrap the missing-terminal rows".to_string(),
                committed_at: "2026-10-14T23:00:00Z".to_string(),
                additions: Some(40),
                deletions: Some(3),
                authors: Some(vec![octo]),
            },
            Commit {
                oid: "05a9899".to_string(),
                headline: String::new(),
                committed_at: "2026-10-14T20:00:00Z".to_string(),
                additions: None,
                deletions: None,
                authors: None,
            },
        ]
    );
    assert_eq!(
        activity.reactions,
        Some(vec![Reaction {
            content: ReactionContent::Heart,
            count: 1,
            actors: vec![],
            viewer_reacted: false,
        }])
    );
    assert!(!activity.cut);
}

#[test]
fn an_activity_without_its_enrichments_still_reads() {
    let value = json!({
        "comments": [],
        "commentCount": 0,
        "commentsTruncated": false,
        "reviewThreads": [],
        "commits": []
    });
    let activity = Activity::read(&value).unwrap();
    assert_eq!(activity.author, None);
    assert_eq!(activity.reviewers, None);
    assert_eq!(activity.reactions, None);
    assert!(!activity.threads_truncated);

    for key in [
        "comments",
        "commentCount",
        "commentsTruncated",
        "reviewThreads",
        "commits",
    ] {
        let mut broken = value.clone();
        broken.as_object_mut().unwrap().remove(key);
        assert!(Activity::read(&broken).is_err(), "{key}");
    }
    let mut thread = activity_value();
    thread["reviewThreads"][0]["line"] = json!(0);
    assert!(Activity::read(&thread).is_err());
    thread["reviewThreads"][0]["line"] = json!(1);
    thread["reviewThreads"][0]["side"] = json!("both");
    assert!(Activity::read(&thread).is_err());
    let mut kind = activity_value();
    kind["comments"][0]["kind"] = json!("commit-comment");
    assert!(Activity::read(&kind).is_err());
}

#[test]
fn checks_read_their_state_and_list() {
    let checks = Checks::read(&json!({
        "state": "merged",
        "checks": [{"name": "macOS", "status": "failure", "description": null, "url": null,
            "required": false}]
    }))
    .unwrap();
    assert_eq!(
        checks,
        Checks {
            state: State::Merged,
            checks: vec![Check {
                name: "macOS".to_string(),
                status: CheckStatus::Failure,
                description: None,
                url: None,
                required: Some(false),
            }],
            cut: false,
        }
    );
    assert!(Checks::read(&json!({"checks": []})).is_err());
}

#[test]
fn an_unavailable_host_says_what_the_server_needs() {
    let error = PullRequestError::from(failed(json!({
        "_tag": "PullRequestUnavailableError",
        "reason": "cli-missing",
        "provider": "github",
        "cause": {"name": "Error", "message": "spawn gh ENOENT"}
    })));
    assert!(matches!(
        error,
        PullRequestError::Unavailable {
            reason: Unavailable::CliMissing,
            provider: Some(Provider::Github),
        }
    ));
    assert_eq!(
        error.to_string(),
        "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`."
    );
    let cases = [
        (
            json!({"reason": "cli-unauthenticated", "provider": "gitlab"}),
            "GitLab CLI is not authenticated. Run `glab auth login` and retry.",
        ),
        (
            json!({"reason": "provider-unsupported"}),
            "Change requests cannot be browsed for this project's host yet.",
        ),
        (
            json!({"reason": "cli-missing", "provider": "unknown"}),
            "The tool this host is read through is not installed or set up.",
        ),
        (
            json!({"reason": "cli-unauthenticated", "provider": "sourcehut"}),
            "This host has no working credentials.",
        ),
        (
            json!({"reason": "cli-unauthenticated", "provider": "bitbucket"}),
            "Bitbucket rejected the configured credentials. Check them in Settings → Source Control.",
        ),
    ];
    for (fields, message) in cases {
        let mut value = fields;
        value["_tag"] = json!("PullRequestUnavailableError");
        let error = PullRequestError::from(failed(value));
        assert!(
            matches!(error, PullRequestError::Unavailable { .. }),
            "{error:?}"
        );
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn every_provider_and_reason_has_its_own_sentence() {
    let providers = [
        Some(Provider::Github),
        Some(Provider::Gitlab),
        Some(Provider::Forgejo),
        Some(Provider::AzureDevops),
        Some(Provider::Bitbucket),
        Some(Provider::Unknown),
        None,
    ];
    let mut sentences = Vec::new();
    for provider in providers {
        for reason in [Unavailable::CliMissing, Unavailable::CliUnauthenticated] {
            sentences.push(unavailable_message(reason, provider));
        }
        assert_eq!(
            unavailable_message(Unavailable::ProviderUnsupported, provider),
            "Change requests cannot be browsed for this project's host yet."
        );
    }
    // Unknown and no provider share the two sentences for any host.
    let distinct: std::collections::HashSet<_> = sentences.iter().collect();
    assert_eq!(distinct.len(), sentences.len() - 2);
}

#[test]
fn other_failures_keep_t3s_message() {
    let not_found = PullRequestError::from(failed(json!({
        "_tag": "PullRequestOperationError",
        "operation": "detail",
        "detail": "Pull request #99 was not found.",
        "reason": "not-found"
    })));
    assert!(matches!(&not_found, PullRequestError::NotFound(_)));
    assert_eq!(not_found.to_string(), "Pull request #99 was not found.");

    let failed_read = PullRequestError::from(failed(json!({
        "_tag": "PullRequestOperationError",
        "operation": "detail",
        "detail": "GitHub answered 502."
    })));
    assert!(matches!(&failed_read, PullRequestError::Rpc(_)));
    assert_eq!(failed_read.to_string(), "GitHub answered 502.");

    let unknown_reason = PullRequestError::from(failed(json!({
        "_tag": "PullRequestUnavailableError",
        "reason": "rate-limited",
        "detail": "Too many requests."
    })));
    assert!(matches!(&unknown_reason, PullRequestError::Rpc(_)));
    assert_eq!(unknown_reason.to_string(), "Too many requests.");

    let lost = PullRequestError::from(RpcError::Disconnected("socket ended".to_string()));
    assert!(matches!(&lost, PullRequestError::Rpc(_)));
    assert_eq!(lost.to_string(), "T3 connection lost: socket ended");
}
