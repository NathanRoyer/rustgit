Pure-rust Git Client

### Example: Cloning, Committing

```rust
use rustgit::*;
use coolssh::create_ed25519_keypair;

let github_account_id = "john.doe@gmail.com";
let (openssh_encoded_pubkey, keypair) = create_ed25519_keypair(github_account_id);

println!("{}", openssh_encoded_pubkey);
// Add this public key to `authorized_keys` on your server
// -> https://github.com/settings/keys

let remote = Remote::new("github.com:22", "git", "NathanRoyer/rustgit.git", &keypair);

let mut repo = Repository::new();
let the_branch = "main";

// we don't need the full history
let clone_depth = Some(1);

// this will clone the branch via SSH
repo.clone(remote, Reference::Branch(the_branch), clone_depth).unwrap();

// enough with this library
repo.stage("src/lib.rs", None).unwrap();

// let's be nice with each other
repo.stage("content.txt", Some(("Hello World!".into(), FileType::RegularFile))).unwrap();
let new_head = repo.commit(
    "I said hello to the world",
    ("John Doe", github_account_id),
    ("John Doe", github_account_id),
    None,
).unwrap();

// this will update the branch via SSH
repo.push(remote, &[(the_branch, new_head)], false).unwrap();
```

### Supported Git Protocols

- Clone: version 2 with optional `shallow` option.
- Push: version 1 with `report-status` and `thin-pack` options.

### Future improvements

- Test against git servers others than Github
- Pack objects as RefDelta
- `fn Repository::log() -> impl Iterator<Item = Commit>`

Feel free to submit pull requests for these.
