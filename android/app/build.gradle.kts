plugins { id("com.android.application") }
android {
    namespace = "com.lightcraft.android"
    compileSdk = 35
    ndkVersion = "28.2.13676358"
    defaultConfig {
        applicationId = "com.lightcraft.android"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.4.0-android"
        ndk { abiFilters += providers.gradleProperty("androidAbi").getOrElse("arm64-v8a") }
    }
    buildTypes {
        getByName("release") {
            // Local sideload build only. Production keys are never stored in this repo.
            signingConfig = signingConfigs.getByName("debug")
            isMinifyEnabled = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    packaging { jniLibs { useLegacyPackaging = false } }
}
