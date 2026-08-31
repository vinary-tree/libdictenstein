import org.gradle.api.publish.maven.MavenPublication
import org.gradle.api.tasks.bundling.Jar
import org.gradle.external.javadoc.StandardJavadocDocletOptions

plugins {
    `java-library`
    `maven-publish`
}

group = "io.vinarytree"
version = "4.0.0-rc.6"

java {
    toolchain.languageVersion = JavaLanguageVersion.of(
        providers.gradleProperty("javaToolchain").orElse("25").get().toInt()
    )
    withSourcesJar()
    withJavadocJar()
}

sourceSets.named("main") {
    resources.srcDir(layout.buildDirectory.dir("native-resources"))
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 22
    options.encoding = "UTF-8"
}

tasks.withType<Javadoc>().configureEach {
    (options as StandardJavadocDocletOptions)
        .addStringOption("Xdoclint:all,-missing", "-quiet")
    (options as StandardJavadocDocletOptions).addBooleanOption("Werror", true)
}

repositories { mavenCentral() }

dependencies {
    api("io.vinarytree:vinary-tree-interop:4.0.0-rc.6")
    testImplementation("io.vinarytree:liblevenshtein:4.0.0-rc.6")
    testImplementation(platform("org.junit:junit-bom:6.1.2"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test {
    useJUnitPlatform()
    jvmArgs("--enable-native-access=ALL-UNNAMED")
    systemProperty(
        "java.library.path",
        providers.gradleProperty("vinaryTree.nativeDir")
            .orElse("../../target/debug:../../../liblevenshtein-rust/target/debug")
            .get()
    )
}

tasks.register<JavaExec>("collectionTraversalProfile") {
    group = "benchmark"
    description = "Run one public-facade collection traversal benchmark arm."
    classpath = sourceSets["main"].runtimeClasspath
    mainClass = "io.vinarytree.libdictenstein.CollectionTraversalProfile"
    jvmArgs("--enable-native-access=ALL-UNNAMED")
    systemProperty(
        "java.library.path",
        providers.gradleProperty("vinaryTree.nativeDir")
            .orElse("../../target/debug:../../../liblevenshtein-rust/target/debug")
            .get()
    )
    doFirst {
        val raw = providers.gradleProperty("profileArgs").orElse("").get().trim()
        setArgs(if (raw.isEmpty()) emptyList<String>() else raw.split(Regex("\\s+")))
    }
}

val nativeResourcesByPlatform = mapOf(
    "linux-x86_64" to "META-INF/native/linux-x86_64/liblibdictenstein.so",
    "linux-aarch64" to "META-INF/native/linux-aarch64/liblibdictenstein.so",
    "macos-aarch64" to "META-INF/native/macos-aarch64/liblibdictenstein.dylib",
    "windows-x86_64" to "META-INF/native/windows-x86_64/libdictenstein.dll",
)
val selectedNativePlatforms = providers.gradleProperty("nativePlatforms")
    .map { value -> value.split(',').map(String::trim).filter(String::isNotEmpty) }
    .orElse(nativeResourcesByPlatform.keys.toList())
val expectedNativeResources = selectedNativePlatforms.map { platforms ->
    platforms.map { platform ->
        nativeResourcesByPlatform[platform]
            ?: error("unsupported native platform requested: $platform")
    }
}

val verifyNativeResources = tasks.register("verifyNativeResources") {
    group = "verification"
    description = "Verify every required native library in the publishable JVM artifact."
    inputs.files(expectedNativeResources.map { resources ->
        resources.map { layout.buildDirectory.file("native-resources/$it") }
    })
    doLast {
        val missing = expectedNativeResources.get().filterNot {
            layout.buildDirectory.file("native-resources/$it").get().asFile.isFile
        }
        check(missing.isEmpty()) { "missing JVM native resources: ${missing.joinToString()}" }
    }
}

tasks.jar {
    dependsOn(verifyNativeResources)
    manifest {
        attributes(
            "Automatic-Module-Name" to "io.vinarytree.libdictenstein",
            "Implementation-Title" to "libdictenstein JVM bindings",
            "Implementation-Version" to project.version,
        )
    }
}

tasks.named<Jar>("sourcesJar") {
    include("**/*.java")
    includeEmptyDirs = false
}

tasks.processResources {
    from(rootProject.file("../../LICENSE")) { into("META-INF") }
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            artifactId = "libdictenstein"
            from(components["java"])
            pom {
                name = "libdictenstein JVM bindings"
                description = "High-performance dictionaries and trie-maps for approximate string matching"
                url = "https://github.com/vinary-tree/libdictenstein"
                licenses {
                    license {
                        name = "Apache License 2.0"
                        url = "https://www.apache.org/licenses/LICENSE-2.0.txt"
                    }
                }
                developers {
                    developer {
                        id = "dylon"
                        name = "Dylon Edwards"
                        email = "dylon.devo@gmail.com"
                    }
                }
                scm {
                    connection = "scm:git:https://github.com/vinary-tree/libdictenstein.git"
                    developerConnection = "scm:git:ssh://git@github.com/vinary-tree/libdictenstein.git"
                    url = "https://github.com/vinary-tree/libdictenstein"
                }
            }
        }
    }
    repositories {
        maven {
            name = "staging"
            url = uri(
                providers.gradleProperty("stagingRepository")
                    .orElse(layout.buildDirectory.dir("staging-deploy").map { it.asFile.absolutePath })
                    .get()
            )
        }
    }
}
