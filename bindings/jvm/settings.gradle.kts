rootProject.name = "libdictenstein"

val interopRoot = providers.gradleProperty("vinaryTreeInteropRoot")
    .orElse(providers.environmentVariable("VINARY_TREE_INTEROP_ROOT"))
    .orElse(file("../../../vinary-tree-interop").absolutePath)
val interopBuild = file("${interopRoot.get()}/bindings/jvm")
if (interopBuild.isDirectory) {
    includeBuild(interopBuild) {
        name = "vinary-tree-interop-jvm"
        dependencySubstitution {
            substitute(module("io.vinarytree:vinary-tree-interop"))
                .using(project(":"))
        }
    }
}

val liblevenshteinRoot = providers.gradleProperty("liblevenshteinRoot")
    .orElse(providers.environmentVariable("LIBLEVENSHTEIN_ROOT"))
    .orElse(file("../../../liblevenshtein-rust").absolutePath)
val liblevenshteinBuild = file("${liblevenshteinRoot.get()}/bindings/jvm")
if (liblevenshteinBuild.isDirectory) {
    includeBuild(liblevenshteinBuild) {
        name = "liblevenshtein-jvm"
        dependencySubstitution {
            substitute(module("io.vinarytree:liblevenshtein"))
                .using(project(":"))
        }
    }
}
