// Permission decisions for object and ancestry inspection. These requests use
// the real delegation classifier; no shell command or repository file is read.
use super::*;

fn assert_allowed(command: &str) {
    assert_eq!(
        read_only_answer(
            "Bash",
            json!({"command":command}),
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            true,
            WORKSPACE,
        ),
        Ok(()),
        "{command}"
    );
}

fn assert_denied(command: &str) {
    let refusal = bash_refusal(command);
    assert_refusal_shape(&refusal, command);
}

#[test]
fn cat_file_inspection_modes_are_admitted() {
    for command in [
        "git cat-file -t HEAD",
        "git cat-file -s HEAD",
        "git cat-file -e HEAD",
        "git cat-file -p HEAD:src/main.rs",
        "git --no-pager cat-file -p HEAD",
        "git -P --no-optional-locks cat-file -t HEAD",
        "git cat-file -t -- HEAD",
        "git cat-file -p 'HEAD^{tree}'",
    ] {
        assert_allowed(command);
    }
}

#[test]
fn cat_file_conversion_batch_and_unknown_modes_stay_refused() {
    for command in [
        "git cat-file",
        "git cat-file -p",
        "git cat-file --textconv HEAD:source.txt",
        "git cat-file --textc HEAD:source.txt",
        "git cat-file --filters HEAD:source.txt",
        "git cat-file --filt HEAD:source.txt",
        "git cat-file --batch",
        "git cat-file --batch='%(objectname)'",
        "git cat-file --batch-check",
        "git cat-file --batch-check='%(objectsize)'",
        "git cat-file --batch-command",
        "git cat-file --batch-all-objects",
        "git cat-file --ba",
        "git cat-file -p HEAD --filters",
        "git cat-file -p --textconv HEAD:source.txt",
        "git cat-file --te\\xtconv HEAD:source.txt",
        "git cat-file -t -p HEAD",
        "git cat-file --follow-symlinks -p HEAD",
        "git cat-file -p HEAD another-object",
        "git cat-file -h",
        "git cat-file --help",
        "git -c core.pager=helper cat-file -p HEAD",
        "git --git-dir=other.git cat-file -t HEAD",
    ] {
        assert_denied(command);
    }
}

#[test]
fn ancestry_queries_are_admitted() {
    for command in [
        "git merge-base HEAD main",
        "git merge-base --all HEAD main",
        "git merge-base -a HEAD main",
        "git merge-base --is-ancestor HEAD main",
        "git merge-base --fork-point main",
        "git merge-base --fork-point main HEAD",
        "git merge-base --octopus HEAD main other",
        "git merge-base --independent HEAD main",
        "git --no-pager merge-base --is-ancestor HEAD main",
    ] {
        assert_allowed(command);
    }
    for command in [
        "git merge-base",
        "git merge-base --is-ancestor HEAD",
        "git merge-base --fork-point main HEAD other",
        "git merge-base --unknown HEAD main",
        "git merge-base --help",
    ] {
        assert_denied(command);
    }
}

#[test]
fn pure_object_and_ref_queries_are_admitted() {
    for command in [
        "git ls-tree HEAD",
        "git ls-tree -r --name-only HEAD src",
        "git ls-tree --format='%(objectname)' HEAD",
        "git rev-list --max-count=1 HEAD",
        "git rev-list -n 1 HEAD -- src/main.rs",
        "git show-ref --head --dereference",
        "git show-ref --verify refs/heads/main",
        "git for-each-ref --format='%(refname) %(objectname)' refs/heads",
        "git for-each-ref --format '%(refname:short)%09%(objectname)' --sort=-authordate refs/heads",
        "git for-each-ref --sort refname --exclude refs/remotes --count=3",
        "git for-each-ref --format='%%(signature:grade)' refs/heads",
        "git for-each-ref --format='%(subject)' --end-of-options refs/heads",
        "git rev-list --format='%H %s' HEAD",
        "git rev-list --pretty=oneline HEAD",
        "git rev-list --format=raw HEAD",
        "git rev-list --format=reference HEAD",
        "git rev-list --pretty='format:%h %an %ad %s' HEAD",
        "git rev-list --format='tformat:%H%+s%-s% s' HEAD",
        "git rev-list --format='format:%%G?%x25G?%n%s' HEAD",
        "git rev-list --oneline HEAD",
    ] {
        assert_allowed(command);
    }
    for command in [
        "git rev-list --alternate-refs HEAD",
        "git rev-list --alternate HEAD",
        "git rev-list --stdin HEAD",
        "git rev-list --stdi HEAD",
        "git rev-list --st HEAD",
        "echo --alternate-refs | git rev-list --stdin HEAD",
        "git rev-list --output=objects.txt HEAD",
        "git ls-tree --output=tree.txt HEAD",
        "git for-each-ref --help",
        "git show-ref --help",
    ] {
        assert_denied(command);
    }
}

#[test]
fn ref_queries_refuse_signature_verification_and_unknown_format_options() {
    for command in [
        "git for-each-ref --format='%(signature:grade)' refs/heads",
        "git for-each-ref --format '%(signature:signer)' refs/heads",
        "git for-each-ref --format='%(*signature:fingerprint)' refs/tags",
        "git for-each-ref --format='%(signature)' refs/heads",
        "git for-each-ref --sort=signature:grade refs/heads",
        "git for-each-ref --sort -signature:grade refs/heads",
        "git for-each-ref --sort='*signature:grade' refs/tags",
        "git for-each-ref --form='%(signature:grade)' refs/heads",
        "git for-each-ref --sor=signature:grade refs/heads",
        "git for-each-ref --color --format='%(signature:grade)' refs/heads",
        "git for-each-ref --format --sort=signature:grade refs/heads",
        "git for-each-ref --unknown refs/heads",
        "git for-each-ref --format='%(future-atom)' refs/heads",
        "git rev-list --format='%G?' HEAD",
        "git rev-list --pretty format:%GS HEAD",
        "git rev-list --show-signature HEAD",
        "git rev-list --show-sig HEAD",
        "git rev-list --pretty=audit HEAD",
        "git rev-list --format=audit HEAD",
        "git rev-list --pretty=one HEAD",
        "git rev-list --pretty HEAD",
        "git rev-list --format audit HEAD",
        "git rev-list --pre=audit HEAD",
        "git rev-list --form=audit HEAD",
        "git rev-list --format='%+G?' HEAD",
        "git rev-list --format='tformat:%-GS' HEAD",
        "git rev-list '--format=% GK' HEAD",
        "git rev-list --format='format:%%G?%+G?' HEAD",
        "git rev-list --format='format:%+%G?' HEAD",
        "git rev-list --format='format:%Q' HEAD",
    ] {
        assert_denied(command);
    }
}

#[test]
fn literal_git_peels_are_admitted_in_revision_positions() {
    for command in [
        "git rev-parse HEAD^{tree}",
        "git rev-parse --verify HEAD^{commit}",
        "git rev-parse HEAD^{}",
        "git rev-parse HEAD^{object}",
        "git rev-parse HEAD^{blob}",
        "git rev-parse release^{tag}",
        "git rev-parse HEAD^{commit}^{tree}",
        "git rev-parse HEAD^{commit}~1",
        "git rev-parse HEAD HEAD^^{tree} HEAD^",
        "git rev-parse HEAD^^^{tree}",
        "git rev-parse HEAD~2^{tree}",
        "git rev-parse HEAD^2^{commit}",
        "git rev-parse HEAD HEAD^{tree} && git status --porcelain --untracked-files=all",
        "git show --format='%H' HEAD^{commit}",
        "git show HEAD^{tree}:src",
        "git diff HEAD^{tree} main^{tree}",
        "git log -n 1 HEAD^{commit}",
        "git cat-file -t HEAD^{tree}",
        "git cat-file -p HEAD^{tree}:docs/example",
        "git cat-file -p HEAD^{commit}^{tree}:src/main.rs",
        "git cat-file -p 'HEAD:docs/example^{tree}'",
        "git merge-base --is-ancestor HEAD^{commit} main^{commit}",
        "git ls-tree -r HEAD^{tree} src",
        "git rev-list --max-count=1 HEAD^{commit}",
        "git for-each-ref --points-at=HEAD^{commit}",
        "git for-each-ref --contains HEAD^{commit}",
        "git for-each-ref --format '%(refname)' --contains HEAD^{commit}",
        "git for-each-ref --exclude refs/remotes --points-at=HEAD^{commit}",
        "git branch --contains=HEAD^{commit}",
        "cd /work/repo && 'git' rev-parse HEAD^{tree}",
        "git rev-parse 'HEAD^{tree}'",
        "git rev-parse \"HEAD^{commit}\"",
    ] {
        assert_allowed(command);
    }
}

#[test]
fn peels_do_not_enable_other_shell_expansion_or_non_revision_positions() {
    for command in [
        "git {rev-parse,status} HEAD",
        "git rev-parse HEAD^{tree,commit}",
        "git rev-parse HEAD^{1..3}",
        "git rev-parse HEAD^{tree}{one,two}",
        "git rev-parse HEAD^{tree$(touch victim)}",
        "git rev-parse HEAD^{tree}; touch victim",
        "git rev-parse HEAD^{unknown}",
        "echo HEAD^{tree}",
        "git status HEAD^{tree}",
        "git grep HEAD^{tree}",
        "git log --format=HEAD^{tree}",
        "git log --format HEAD^{tree}",
        "git show HEAD -- HEAD^{tree}",
        "git ls-tree HEAD src^{tree}",
        "git cat-file -p HEAD:docs/example^{tree}",
        "git show HEAD:docs/example^{tree}",
        "git rev-parse HEAD:docs/example^{}",
        "git cat-file -p HEAD^{tree}:docs/example^{tree}",
        "git for-each-ref --contains=HEAD:docs/example^{tree}",
        "git for-each-ref HEAD^{tree}",
        "git for-each-ref --format --points-at=HEAD^{tree}",
        "git for-each-ref --form --points-at=HEAD^{commit}",
        "git for-each-ref --for --points-at=HEAD^{commit}",
        "git for-each-ref --sor --points-at=HEAD^{commit}",
        "git for-each-ref --cou --points-at=HEAD^{commit}",
        "git for-each-ref --exclude --points-at=HEAD^{commit}",
        "git for-each-ref --start-after --points-at=HEAD^{commit}",
        "git for-each-ref --end-of-options --points-at=HEAD^{commit}",
        "git for-each-ref --unknown --points-at=HEAD^{commit}",
        "git for-each-ref -- refs/heads^{tree}",
        "cd /work/other && git rev-parse HEAD^{tree}",
        "cd /work/other && 'git' cat-file -t HEAD^{commit}",
        "git rev-parse HEAD^{tree} > tree.txt",
        "git rev-parse HEAD^{tree} && touch victim",
    ] {
        assert_denied(command);
    }
}
