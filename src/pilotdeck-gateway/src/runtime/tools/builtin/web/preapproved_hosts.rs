//! Pre-approved hosts for `web_fetch` (baseline `tool/builtin/web/preapprovedHosts.ts`).
//! Hosts here only switch the secondary-model guidelines to the documentation
//! wording; they do not bypass permissions. The table is data transcribed
//! mechanically from the baseline and checked entry by entry against the
//! real `PREAPPROVED_ENTRIES` in the fixture replay.

/// `PreapprovedEntry`: lowercased host, optional path prefix, `*.<host>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreapprovedEntry {
    pub host: &'static str,
    pub path_prefix: Option<&'static str>,
    pub allow_subdomains: bool,
}

const fn entry(
    host: &'static str,
    allow_subdomains: bool,
    path_prefix: Option<&'static str>,
) -> PreapprovedEntry {
    PreapprovedEntry {
        host,
        path_prefix,
        allow_subdomains,
    }
}

/// `PREAPPROVED_ENTRIES` in baseline order (136 entries).
pub const PREAPPROVED_ENTRIES: [PreapprovedEntry; 136] = [
    entry("docs.anthropic.com", false, None),
    entry("anthropic.com", false, None),
    entry("developer.mozilla.org", false, None),
    entry("kernel.org", true, None),
    entry("linux.die.net", false, None),
    entry("stackoverflow.com", false, None),
    entry("wikipedia.org", true, None),
    entry("npmjs.com", true, None),
    entry("pypi.org", false, None),
    entry("rubygems.org", false, None),
    entry("crates.io", false, None),
    entry("pkg.go.dev", false, None),
    entry("packagist.org", false, None),
    entry("central.sonatype.com", false, None),
    entry("github.com", false, None),
    entry("gitlab.com", false, None),
    entry("bitbucket.org", false, None),
    entry("codeberg.org", false, None),
    entry("sourceforge.net", false, None),
    entry("aws.amazon.com", false, Some("/documentation")),
    entry("docs.aws.amazon.com", false, None),
    entry("cloud.google.com", false, Some("/docs")),
    entry("learn.microsoft.com", false, None),
    entry("docs.microsoft.com", false, None),
    entry("azure.microsoft.com", false, Some("/en-us/documentation")),
    entry("vercel.com", false, Some("/docs")),
    entry("netlify.com", false, Some("/docs")),
    entry("kubernetes.io", false, None),
    entry("docs.docker.com", false, None),
    entry("helm.sh", false, None),
    entry("istio.io", false, None),
    entry("nodejs.org", true, None),
    entry("deno.land", false, None),
    entry("bun.sh", false, None),
    entry("python.org", true, None),
    entry("go.dev", false, None),
    entry("rust-lang.org", true, None),
    entry("ruby-lang.org", true, None),
    entry("ruby-doc.org", false, None),
    entry("php.net", true, None),
    entry("swift.org", false, None),
    entry("kotlinlang.org", false, None),
    entry("scala-lang.org", false, None),
    entry("haskell.org", true, None),
    entry("elixir-lang.org", false, None),
    entry("dart.dev", false, None),
    entry("flutter.dev", false, None),
    entry("react.dev", false, None),
    entry("reactjs.org", false, None),
    entry("vuejs.org", true, None),
    entry("angular.io", false, None),
    entry("angular.dev", false, None),
    entry("svelte.dev", false, None),
    entry("kit.svelte.dev", false, None),
    entry("nextjs.org", false, None),
    entry("remix.run", false, None),
    entry("nuxt.com", false, None),
    entry("nuxtjs.org", false, None),
    entry("astro.build", false, None),
    entry("solidjs.com", false, None),
    entry("qwik.dev", false, None),
    entry("expressjs.com", false, None),
    entry("fastify.dev", false, None),
    entry("nestjs.com", false, None),
    entry("django-project.com", false, None),
    entry("djangoproject.com", false, None),
    entry("flask.palletsprojects.com", false, None),
    entry("fastapi.tiangolo.com", false, None),
    entry("rubyonrails.org", false, None),
    entry("guides.rubyonrails.org", false, None),
    entry("spring.io", false, None),
    entry("laravel.com", false, None),
    entry("postgresql.org", true, None),
    entry("mysql.com", false, Some("/doc")),
    entry("dev.mysql.com", false, None),
    entry("mariadb.com", false, Some("/kb")),
    entry("mariadb.org", false, Some("/documentation")),
    entry("sqlite.org", false, None),
    entry("redis.io", false, None),
    entry("mongodb.com", false, Some("/docs")),
    entry("docs.mongodb.com", false, None),
    entry("cassandra.apache.org", false, None),
    entry("neo4j.com", false, Some("/docs")),
    entry("duckdb.org", false, None),
    entry("clickhouse.com", false, Some("/docs")),
    entry("docs.github.com", false, None),
    entry("circleci.com", false, Some("/docs")),
    entry("travis-ci.com", false, Some("/docs")),
    entry("jenkins.io", false, Some("/doc")),
    entry("prometheus.io", false, None),
    entry("grafana.com", false, Some("/docs")),
    entry("elastic.co", false, Some("/guide")),
    entry("opentelemetry.io", false, None),
    entry("w3.org", true, None),
    entry("rfc-editor.org", false, None),
    entry("ietf.org", true, None),
    entry("tools.ietf.org", false, None),
    entry("datatracker.ietf.org", false, None),
    entry("tc39.es", false, None),
    entry("huggingface.co", false, None),
    entry("pytorch.org", false, None),
    entry("tensorflow.org", true, None),
    entry("scikit-learn.org", false, None),
    entry("openai.com", false, Some("/docs")),
    entry("platform.openai.com", false, None),
    entry("code.visualstudio.com", false, None),
    entry("marketplace.visualstudio.com", false, None),
    entry("jetbrains.com", false, Some("/help")),
    entry("vim.org", false, None),
    entry("emacs.org", false, None),
    entry("neovim.io", false, None),
    entry("manpages.debian.org", false, None),
    entry("manpages.ubuntu.com", false, None),
    entry("ss64.com", false, None),
    entry("tldp.org", false, None),
    entry("linuxfromscratch.org", false, None),
    entry("owasp.org", false, None),
    entry("cve.mitre.org", false, None),
    entry("nvd.nist.gov", false, None),
    entry("cwe.mitre.org", false, None),
    entry("chromium.org", true, None),
    entry("webkit.org", false, None),
    entry("mozilla.org", true, None),
    entry("caniuse.com", false, None),
    entry("npmtrends.com", false, None),
    entry("readthedocs.io", true, None),
    entry("readthedocs.org", true, None),
    entry("gitbook.io", true, None),
    entry("notion.so", false, Some("/help")),
    entry("supabase.com", false, Some("/docs")),
    entry("firebase.google.com", false, Some("/docs")),
    entry("stripe.com", false, Some("/docs")),
    entry("twilio.com", false, Some("/docs")),
    entry("sendgrid.com", false, Some("/docs")),
    entry("auth0.com", false, Some("/docs")),
    entry("okta.com", false, Some("/docs")),
];

/// `isPreapprovedHost(hostname, pathname)`: case-insensitive host equality or
/// `*.<host>` when allowed; then a case-sensitive path prefix, if any.
pub fn is_preapproved_host(hostname: &str, pathname: &str) -> bool {
    let lower = super::js_lower(hostname);
    PREAPPROVED_ENTRIES.iter().any(|entry| {
        let host_match = lower == entry.host
            || (entry.allow_subdomains && lower.ends_with(&format!(".{}", entry.host)));
        host_match
            && entry
                .path_prefix
                .is_none_or(|prefix| pathname.starts_with(prefix))
    })
}

/// `isPreapprovedUrl(url)`: false for anything `new URL` rejects.
pub fn is_preapproved_url(url: &str) -> bool {
    super::url_validation::parse(url).is_some_and(|parsed| {
        is_preapproved_host(&super::url_validation::hostname(&parsed), parsed.path())
    })
}
