// @generated SignedSource<<cb17cfee46cd3f890d0e413c4ae99a03>>
// DO NOT EDIT THIS FILE MANUALLY!
// This file is a mechanical copy of the version in the configerator repo. To
// modify it, edit the copy in the configerator repo instead and copy it over by
// running the following in your fbcode directory:
//
// configerator-thrift-updater scm/mononoke/blobimport/state.thrift

namespace py configerator.blobimport_state.state

typedef i64 RepoId
typedef string RepoName

# TODO:  Remove after updating blobimport
struct BlobimportState {
  1: map<RepoId, bool> running,
}

# TODO:  Remove after updating blobimport
struct RepoNames {
  1: map<string, RepoId> mapping,
}

enum BlobimportStatus {
  STOPPED = 0,
  RUNNING = 1,
}

struct Repository {
  1: RepoId id,
  2: RepoName name,
  3: BlobimportStatus blobimport_status,
}

struct Blobimport {
  1: list<Repository> repositories,
}