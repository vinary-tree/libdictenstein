rootProject.name = "libdictenstein"

includeBuild("../../../liblevenshtein-rust/vinary-tree-interop/bindings/jvm") {
    name = "vinary-tree-interop-jvm"
    dependencySubstitution {
        substitute(module("io.vinarytree:vinary-tree-interop"))
            .using(project(":"))
    }
}

includeBuild("../../../liblevenshtein-rust/bindings/jvm") {
    name = "liblevenshtein-jvm"
    dependencySubstitution {
        substitute(module("io.vinarytree:liblevenshtein"))
            .using(project(":"))
    }
}
