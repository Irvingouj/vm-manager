// The documentation site's pages, in the order of the sidebar.
//
// Each page is made of parts:
//   { md: "..." }                         a few words of this site's own (only to join and point)
//   { file: "content/x.md" }              a file of site/, whole
//   { repo: "docs/getting-started.md" }   a document of the repository, whole
//   { repo, sections: ["## heading"] }    some of its sections, by their heading as written
//   { repo, preamble: true }              its text before the first "## " heading
//   { sdk: "sdk/index.d.ts" }             the SDK's types, cut at their "// ----- name" lines
//
// The repository's documents are read as they are at build time: nothing of
// them is copied here, so the site says what the repository says.

export const groups = [
  {
    title: "Start here",
    pages: [
      { slug: "", title: "Overview", parts: [{ file: "content/overview.md" }] },
      {
        slug: "getting-started",
        title: "Getting started",
        parts: [{ repo: "docs/getting-started.md" }],
      },
    ],
  },
  {
    title: "For Builders",
    pages: [
      {
        slug: "builders/local-run",
        title: "Making it and running it locally",
        parts: [
          { file: "content/builders-local-run.md" },
          {
            repo: "docs/getting-started.md",
            sections: ["## 6. Make a project", "## 7. Run it on your computer"],
          },
        ],
      },
      {
        slug: "builders/deploying",
        title: "Deploying",
        parts: [
          { file: "content/builders-deploying.md" },
          { repo: "docs/getting-started.md", sections: ["## 8. Deploy it"] },
        ],
      },
      {
        slug: "builders/sharing",
        title: "Sharing",
        parts: [
          { file: "content/builders-sharing.md" },
          { repo: "docs/getting-started.md", sections: ["## 9. Share it"] },
        ],
      },
      { slug: "builders/commands", title: "The tool's commands", parts: [{ file: "content/commands.md" }] },
      {
        slug: "builders/sdk",
        title: "The SDK's types",
        parts: [{ file: "content/sdk.md" }, { sdk: "sdk/index.d.ts" }],
      },
    ],
  },
  {
    title: "For Platform administrators",
    pages: [
      {
        slug: "admin/install",
        title: "Installing with Compose",
        parts: [
          { repo: "deploy/README.md", preamble: true },
          {
            repo: "deploy/README.md",
            sections: [
              "## 1. The files",
              "## 2. The values",
              "## 3. The certificate",
              "## 4. Start it",
              "## 5. The setup credential",
              "## If the first start was interrupted",
              "## The bundled Postgres",
            ],
          },
        ],
      },
      {
        slug: "admin/oidc",
        title: "Connecting OIDC",
        parts: [
          {
            repo: "deploy/README.md",
            sections: [
              "## 6. Register the platform at the identity provider",
              "## 7. Connect OIDC",
              "## 8. Sign in",
              "## Your company's own certificate authority",
            ],
          },
        ],
      },
      {
        slug: "admin/tls",
        title: "TLS",
        parts: [
          { file: "content/admin-tls.md" },
          {
            repo: "deploy/README.md",
            sections: ["## Your own reverse proxy", "## The program serves TLS itself"],
          },
        ],
      },
      {
        slug: "admin/postgres",
        title: "Your own Postgres",
        parts: [{ repo: "deploy/README.md", sections: ["## Your own Postgres"] }],
      },
      {
        slug: "admin/updating",
        title: "Upgrading and stopping",
        parts: [
          {
            repo: "deploy/README.md",
            sections: ["## Upgrading", "## Stopping", "## How it is hardened", "## Why these versions of Docker"],
          },
        ],
      },
    ],
  },
  {
    title: "Reference",
    pages: [
      { slug: "releases", title: "Release notes", parts: [{ release: true }] },
      { slug: "glossary", title: "Glossary", parts: [{ repo: "CONTEXT.md" }] },
    ],
  },
];
