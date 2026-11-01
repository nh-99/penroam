allprojects {
    repositories {
        google()
        mavenCentral()
        // M00: official Onyx/BOOX repository (HTTPS endpoint fails TLS with modern JDKs).
        maven {
            url = uri("http://repo.boox.com/repository/maven-public/")
            isAllowInsecureProtocol = true
            content {
                includeGroupByRegex("com\\.onyx.*")
                includeGroup("pub.devrel")
                includeGroup("com.tencent")
                includeGroup("com.jakewharton.hugo.fix")
            }
        }
    }
}

val newBuildDir: Directory =
    rootProject.layout.buildDirectory
        .dir("../../build")
        .get()
rootProject.layout.buildDirectory.value(newBuildDir)

subprojects {
    val newSubprojectBuildDir: Directory = newBuildDir.dir(project.name)
    project.layout.buildDirectory.value(newSubprojectBuildDir)
}
subprojects {
    project.evaluationDependsOn(":app")
}

tasks.register<Delete>("clean") {
    delete(rootProject.layout.buildDirectory)
}
